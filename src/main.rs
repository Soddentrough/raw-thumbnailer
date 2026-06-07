use anyhow::Result;
use image::{ImageBuffer, Rgb};
use log::{error, info};
use rawler::decoders::RawDecodeParams;
use rawler::imgop::develop::RawDevelop;
use simplelog::{Config, LevelFilter, SimpleLogger};
use std::path::Path;
use url::Url;
use zbus::{dbus_interface, ConnectionBuilder};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::sync::Semaphore;

#[derive(clap::Parser, Debug)]
#[command(author, version, about = "A raw image thumbnailer")]
struct Cli {
    /// Specify the size of the thumbnail (scaled to fit size x size)
    #[arg(short = 's', default_value_t = 512)]
    size: u32,

    /// Start the standard D-Bus service
    #[arg(long = "dbus")]
    dbus: bool,

    /// Input RAW file path
    #[arg(required_unless_present = "dbus")]
    input: Option<String>,

    /// Output thumbnail image path
    #[arg(required_unless_present = "dbus")]
    output: Option<String>,
}

struct Thumbnailer {
    cancelled_handles: Arc<Mutex<HashSet<u32>>>,
    next_handle: AtomicU32,
    semaphore: Arc<Semaphore>,
}

impl Thumbnailer {
    fn new() -> Self {
        info!("Setting concurrency limit to 4");
        Self {
            cancelled_handles: Arc::new(Mutex::new(HashSet::new())),
            next_handle: AtomicU32::new(1),
            semaphore: Arc::new(Semaphore::new(4)),
        }
    }
}

#[dbus_interface(name = "org.freedesktop.thumbnails.Thumbnailer1")]
impl Thumbnailer {
    async fn queue(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        uris: Vec<String>,
        mime_types: Vec<String>,
        flavor: String,
        _scheduler: String,
        handle_to_dequeue: u32,
    ) -> std::result::Result<u32, zbus::fdo::Error> {
        let handle = self.next_handle.fetch_add(1, Ordering::SeqCst);

        if handle_to_dequeue > 0 {
            let mut cancelled = self.cancelled_handles.lock().unwrap();
            cancelled.insert(handle_to_dequeue);
        }

        let connection = connection.clone();
        let cancelled_handles = self.cancelled_handles.clone();
        let semaphore = self.semaphore.clone();

        tokio::spawn(async move {
            // Emit Started signal
            let _ = connection.emit_signal(
                None::<&str>,
                "/org/freedesktop/thumbnails/Thumbnailer1",
                "org.freedesktop.thumbnails.Thumbnailer1",
                "Started",
                &handle,
            ).await;

            let mut join_set = tokio::task::JoinSet::new();

            for (uri, _mime_type) in uris.into_iter().zip(mime_types.into_iter()) {
                let connection = connection.clone();
                let cancelled_handles = cancelled_handles.clone();
                let semaphore = semaphore.clone();
                let flavor = flavor.clone();

                join_set.spawn(async move {
                    // Check if cancelled before starting
                    {
                        let cancelled = cancelled_handles.lock().unwrap();
                        if cancelled.contains(&handle) {
                            info!("Queue handle {} was cancelled. Skipping {}", handle, uri);
                            return;
                        }
                    }

                    info!("Processing queue URI: {}", uri);
                    let parsed_url = match Url::parse(&uri) {
                        Ok(u) => u,
                        Err(e) => {
                            error!("Invalid URI format: {}", e);
                            let _ = connection.emit_signal(
                                None::<&str>,
                                "/org/freedesktop/thumbnails/Thumbnailer1",
                                "org.freedesktop.thumbnails.Thumbnailer1",
                                "Error",
                                &(handle, vec![uri.clone()], 1u32, &format!("Invalid URI: {}", e)),
                            ).await;
                            return;
                        }
                    };

                    let input_path = match parsed_url.to_file_path() {
                        Ok(p) => p,
                        Err(_) => {
                            error!("URI is not a valid file path: {}", uri);
                            let _ = connection.emit_signal(
                                None::<&str>,
                                "/org/freedesktop/thumbnails/Thumbnailer1",
                                "org.freedesktop.thumbnails.Thumbnailer1",
                                "Error",
                                &(handle, vec![uri.clone()], 1u32, "URI is not a valid file path"),
                            ).await;
                            return;
                        }
                    };

                    // Determine cache directory based on flavor
                    let dir_name = match flavor.as_str() {
                        "normal" => "normal",
                        "large" => "large",
                        "xlarge" => "xlarge",
                        "xxlarge" => "xxlarge",
                        _ => "large",
                    };

                    let size = match flavor.as_str() {
                        "normal" => 128,
                        "large" => 256,
                        "xlarge" => 512,
                        "xxlarge" => 1024,
                        _ => 256,
                    };

                    let cache_dir = match directories::BaseDirs::new() {
                        Some(dirs) => dirs.cache_dir().join("thumbnails").join(dir_name),
                        None => {
                            error!("Failed to find user cache directory");
                            let _ = connection.emit_signal(
                                None::<&str>,
                                "/org/freedesktop/thumbnails/Thumbnailer1",
                                "org.freedesktop.thumbnails.Thumbnailer1",
                                "Error",
                                &(handle, vec![uri.clone()], 2u32, "Failed to find user cache directory"),
                            ).await;
                            return;
                        }
                    };

                    if let Err(e) = std::fs::create_dir_all(&cache_dir) {
                        error!("Failed to create cache directory {:?}: {}", cache_dir, e);
                        let _ = connection.emit_signal(
                            None::<&str>,
                            "/org/freedesktop/thumbnails/Thumbnailer1",
                            "org.freedesktop.thumbnails.Thumbnailer1",
                            "Error",
                            &(handle, vec![uri.clone()], 2u32, &format!("Failed to create cache directory: {}", e)),
                        ).await;
                        return;
                    }

                    // Output filename: md5(uri).png
                    let hash = md5::compute(uri.as_bytes());
                    let md5_hex = format!("{:x}", hash);
                    let output_path = cache_dir.join(format!("{}.png", md5_hex));

                    // Read mtime
                    let mtime_str = match std::fs::metadata(&input_path).and_then(|m| m.modified()) {
                        Ok(modified) => {
                            match modified.duration_since(std::time::UNIX_EPOCH) {
                                Ok(duration) => duration.as_secs().to_string(),
                                Err(_) => "0".to_string(),
                            }
                        }
                        Err(e) => {
                            error!("Failed to read metadata for {:?}: {}", input_path, e);
                            let _ = connection.emit_signal(
                                None::<&str>,
                                "/org/freedesktop/thumbnails/Thumbnailer1",
                                "org.freedesktop.thumbnails.Thumbnailer1",
                                "Error",
                                &(handle, vec![uri.clone()], 3u32, &format!("Failed to read file metadata: {}", e)),
                            ).await;
                            return;
                        }
                    };

                    // Check if cancelled again before acquiring permit/generating
                    {
                        let cancelled = cancelled_handles.lock().unwrap();
                        if cancelled.contains(&handle) {
                            info!("Queue handle {} was cancelled. Skipping {}", handle, uri);
                            return;
                        }
                    }

                    // Concurrency limiting: Acquire permit from semaphore before generating thumbnail.
                    let permit = match semaphore.acquire_owned().await {
                        Ok(p) => p,
                        Err(e) => {
                            error!("Failed to acquire concurrency permit: {}", e);
                            let _ = connection.emit_signal(
                                None::<&str>,
                                "/org/freedesktop/thumbnails/Thumbnailer1",
                                "org.freedesktop.thumbnails.Thumbnailer1",
                                "Error",
                                &(handle, vec![uri.clone()], 5u32, &format!("Failed to acquire concurrency permit: {}", e)),
                            ).await;
                            return;
                        }
                    };

                    let input_path_clone = input_path.clone();
                    let output_path_clone = output_path.clone();
                    let uri_clone = uri.clone();
                    let mtime_str_clone = mtime_str.clone();

                    let generate_result = tokio::task::spawn_blocking(move || {
                        let _permit = permit;
                        generate_thumbnail(&input_path_clone, size).and_then(|thumbnail| {
                            save_png_with_metadata(&output_path_clone, &thumbnail, &uri_clone, &mtime_str_clone)
                        })
                    }).await;

                    match generate_result {
                        Ok(Ok(())) => {
                            info!("Thumbnail successfully generated & saved to {:?}", output_path);
                            let _ = connection.emit_signal(
                                None::<&str>,
                                "/org/freedesktop/thumbnails/Thumbnailer1",
                                "org.freedesktop.thumbnails.Thumbnailer1",
                                "Ready",
                                &(handle, vec![uri.clone()]),
                            ).await;
                        }
                        Ok(Err(e)) => {
                            error!("Failed to generate thumbnail for {:?}: {}", input_path, e);
                            let _ = connection.emit_signal(
                                None::<&str>,
                                "/org/freedesktop/thumbnails/Thumbnailer1",
                                "org.freedesktop.thumbnails.Thumbnailer1",
                                "Error",
                                &(handle, vec![uri.clone()], 5u32, &format!("Failed to generate thumbnail: {}", e)),
                            ).await;
                        }
                        Err(e) => {
                            error!("Blocking task panicked: {}", e);
                            let _ = connection.emit_signal(
                                None::<&str>,
                                "/org/freedesktop/thumbnails/Thumbnailer1",
                                "org.freedesktop.thumbnails.Thumbnailer1",
                                "Error",
                                &(handle, vec![uri.clone()], 5u32, "Internal thumbnailer panic"),
                            ).await;
                        }
                    }
                });
            }

            // Await all spawned tasks in the join set
            while let Some(res) = join_set.join_next().await {
                if let Err(e) = res {
                    error!("Join error in queue worker: {}", e);
                }
            }

            // Emit Finished signal
            let _ = connection.emit_signal(
                None::<&str>,
                "/org/freedesktop/thumbnails/Thumbnailer1",
                "org.freedesktop.thumbnails.Thumbnailer1",
                "Finished",
                &handle,
            ).await;
        });

        Ok(handle)
    }

    async fn dequeue(&self, handle: u32) -> std::result::Result<(), zbus::fdo::Error> {
        let mut cancelled = self.cancelled_handles.lock().unwrap();
        cancelled.insert(handle);
        Ok(())
    }

    async fn get_supported(&self) -> std::result::Result<(Vec<String>, Vec<String>), zbus::fdo::Error> {
        let uri_schemes = vec!["file".to_string()];
        let mime_types = vec![
            "image/x-3fr".to_string(),
            "image/x-adobe-dng".to_string(),
            "image/x-arw".to_string(),
            "image/x-bay".to_string(),
            "image/x-canon-cr2".to_string(),
            "image/x-canon-cr3".to_string(),
            "image/x-canon-crw".to_string(),
            "image/x-cap".to_string(),
            "image/x-cr2".to_string(),
            "image/x-cr3".to_string(),
            "image/x-crw".to_string(),
            "image/x-dcr".to_string(),
            "image/x-dcs".to_string(),
            "image/x-dng".to_string(),
            "image/x-drf".to_string(),
            "image/x-eip".to_string(),
            "image/x-erf".to_string(),
            "image/x-fff".to_string(),
            "image/x-fuji-raf".to_string(),
            "image/x-iiq".to_string(),
            "image/x-k25".to_string(),
            "image/x-kdc".to_string(),
            "image/x-mef".to_string(),
            "image/x-minolta-mrw".to_string(),
            "image/x-mos".to_string(),
            "image/x-mrw".to_string(),
            "image/x-nef".to_string(),
            "image/x-nikon-nef".to_string(),
            "image/x-nrw".to_string(),
            "image/x-olympus-orf".to_string(),
            "image/x-orf".to_string(),
            "image/x-panasonic-raw".to_string(),
            "image/x-panasonic-rw2".to_string(),
            "image/x-pef".to_string(),
            "image/x-pentax-pef".to_string(),
            "image/x-ptx".to_string(),
            "image/x-pxn".to_string(),
            "image/x-r3d".to_string(),
            "image/x-raf".to_string(),
            "image/x-raw".to_string(),
            "image/x-rw2".to_string(),
            "image/x-rwl".to_string(),
            "image/x-rwz".to_string(),
            "image/x-samsung-srw".to_string(),
            "image/x-sigma-x3f".to_string(),
            "image/x-sony-arw".to_string(),
            "image/x-sony-sr2".to_string(),
            "image/x-sony-srf".to_string(),
            "image/x-sr2".to_string(),
            "image/x-srf".to_string(),
            "image/x-srw".to_string(),
            "image/x-x3f".to_string(),
        ];
        Ok((uri_schemes, mime_types))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging to stderr (compatible with bwrap sandboxes and systemd journal)
    let _ = SimpleLogger::init(LevelFilter::Info, Config::default());

    let cli = <Cli as clap::Parser>::parse();

    if cli.dbus {
        info!("Starting D-Bus service...");
        let thumbnailer = Thumbnailer::new();
        let _conn = ConnectionBuilder::session()?
            .name("org.freedesktop.thumbnails.Thumbnailer1")?
            .serve_at("/org/freedesktop/thumbnails/Thumbnailer1", thumbnailer)?
            .build()
            .await?;

        // Keep the service running
        std::future::pending::<()>().await;
    } else {
        let input_path_str = cli.input.ok_or_else(|| anyhow::anyhow!("Input file required"))?;
        let output_path_str = cli.output.ok_or_else(|| anyhow::anyhow!("Output file required"))?;
        let size = cli.size;

        let input_path = Path::new(&input_path_str);
        let output_path = Path::new(&output_path_str);

        info!("Generating thumbnail for {:?} with size {}...", input_path, size);
        match generate_thumbnail(input_path, size) {
            Ok(thumbnail) => {
                info!("Saving thumbnail to {:?}...", output_path);
                // Extract modification time
                let mtime_str = match std::fs::metadata(input_path).and_then(|m| m.modified()) {
                    Ok(modified) => {
                        match modified.duration_since(std::time::UNIX_EPOCH) {
                            Ok(duration) => duration.as_secs().to_string(),
                            Err(_) => "0".to_string(),
                        }
                    }
                    Err(_) => "0".to_string(),
                };
                // Make URI
                let uri = match Url::from_file_path(std::fs::canonicalize(input_path)?) {
                    Ok(u) => u.to_string(),
                    Err(_) => format!("file://{}", input_path.to_string_lossy()),
                };
                if let Err(e) = save_png_with_metadata(output_path, &thumbnail, &uri, &mtime_str) {
                    error!("Failed to save thumbnail: {}", e);
                    return Err(e);
                }
                info!("Thumbnail created successfully.");
            }
            Err(e) => {
                let msg = format!("Failed to generate thumbnail for {:?}: {}", input_path, e);
                error!("{}", msg);
                eprintln!("{}", msg);
                return Err(e);
            }
        }
    }

    Ok(())
}

fn generate_thumbnail(path: &Path, size: u32) -> Result<ImageBuffer<Rgb<u8>, Vec<u8>>> {
    let params = RawDecodeParams { image_index: 0 };
    
    // Open the raw source file exactly once.
    let raw_source = rawler::rawsource::RawSource::new(path)?;
    let decoder = rawler::get_decoder(&raw_source)?;
    
    // Extract metadata & orientation once from the decoder.
    let orient = if let Ok(meta) = decoder.raw_metadata(&raw_source, &params) {
        image::metadata::Orientation::from_exif(meta.exif.orientation.unwrap_or(1) as u8)
            .unwrap_or(image::metadata::Orientation::NoTransforms)
    } else {
        image::metadata::Orientation::NoTransforms
    };

    // 1. Try to decode the preview image from the same decoder.
    if let Ok(Some(preview)) = decoder.preview_image(&raw_source, &params) {
        info!("Successfully extracted preview for {:?}", path);
        let mut thumbnail = preview.thumbnail(size, size);
        thumbnail.apply_orientation(orient);
        return Ok(thumbnail.to_rgb8());
    }

    // 2. Try to decode the full image from the same decoder.
    if let Ok(Some(preview)) = decoder.full_image(&raw_source, &params) {
        info!("Successfully decoded full preview image for {:?}", path);
        let mut thumbnail = preview.thumbnail(size, size);
        thumbnail.apply_orientation(orient);
        return Ok(thumbnail.to_rgb8());
    }

    // 3. Fallback: decode full raw image and develop it
    info!("No preview found, decoding full image for {:?}", path);
    let raw_image = decoder.raw_image(&raw_source, &params, false)?;
    let developed_image = RawDevelop::default().develop_intermediate(&raw_image)?;
    let dynamic_image = developed_image
        .to_dynamic_image()
        .ok_or_else(|| anyhow::anyhow!("Failed to convert to dynamic image"))?;
        
    let mut thumbnail = dynamic_image.thumbnail(size, size);
    thumbnail.apply_orientation(orient);
    Ok(thumbnail.to_rgb8())
}

fn save_png_with_metadata(
    output_path: &Path,
    img: &ImageBuffer<Rgb<u8>, Vec<u8>>,
    uri: &str,
    mtime: &str,
) -> Result<()> {
    use std::fs::File;
    use std::io::BufWriter;
    use png::text_metadata::TEXtChunk;

    let file = File::create(output_path)?;
    let ref mut w = BufWriter::new(file);

    let mut info = png::Info::default();
    info.width = img.width();
    info.height = img.height();
    info.color_type = png::ColorType::Rgb;
    info.bit_depth = png::BitDepth::Eight;
    info.uncompressed_latin1_text = vec![
        TEXtChunk {
            keyword: "Thumb::URI".to_string(),
            text: uri.to_string(),
        },
        TEXtChunk {
            keyword: "Thumb::MTime".to_string(),
            text: mtime.to_string(),
        },
    ];

    let encoder = png::Encoder::with_info(w, info)?;
    let mut writer = encoder.write_header()?;
    writer.write_image_data(img.as_raw())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn test_cli_parsing_dbus() {
        let args = vec!["raw-thumbnailer", "--dbus"];
        let cli = Cli::try_parse_from(args).unwrap();
        assert!(cli.dbus);
    }

    #[test]
    fn test_cli_parsing_args() {
        let args = vec!["raw-thumbnailer", "-s", "256", "input.nef", "output.png"];
        let cli = Cli::try_parse_from(args).unwrap();
        assert_eq!(cli.size, 256);
        assert_eq!(cli.input.as_deref(), Some("input.nef"));
        assert_eq!(cli.output.as_deref(), Some("output.png"));
        assert!(!cli.dbus);
    }

    #[test]
    fn test_cli_parsing_missing_fields() {
        let args = vec!["raw-thumbnailer", "input.nef"];
        let result = Cli::try_parse_from(args);
        assert!(result.is_err());
    }
}
