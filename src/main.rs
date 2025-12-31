use anyhow::Result;
use image::{ImageBuffer, Rgb};
use log::{error, info};
use rawler::analyze::extract_preview_pixels;
use rawler::decoders::RawDecodeParams;
use rawler::imgop::develop::RawDevelop;
use simplelog::{Config, LevelFilter, SimpleLogger};
use std::env;

use std::path::Path;
use zbus::{dbus_interface, ConnectionBuilder};

struct Thumbnailer;

#[dbus_interface(name = "org.gnome.RawThumbnailer")]
impl Thumbnailer {
    fn thumbnail(
        &self,
        uri: &str,
        output_path: &str,
    ) -> std::result::Result<(), zbus::fdo::Error> {
        info!("Thumbnail request for URI: {}", uri);
        let path_str = uri.trim_start_matches("file://");
        let input_path = Path::new(path_str);
        let output_path = Path::new(output_path);

        match generate_thumbnail(input_path, 256) {
            Ok(thumbnail) => {
                info!("Saving thumbnail to {:?}...", output_path);
                if let Err(e) = thumbnail.save(output_path) {
                    error!("Failed to save thumbnail: {}", e);
                    return Err(zbus::fdo::Error::Failed(e.to_string()));
                }
                info!("Thumbnail created successfully.");
                Ok(())
            }
            Err(e) => {
                error!("Failed to generate thumbnail for {:?}: {}", input_path, e);
                Err(zbus::fdo::Error::Failed(e.to_string()))
            }
        }
    }
}

#[tokio::main(flavor = "current_thread")]

async fn main() -> Result<()> {
    // Initialize logging to stderr (compatible with bwrap sandboxes and systemd journal)
    let _ = SimpleLogger::init(LevelFilter::Info, Config::default());

    let args: Vec<String> = env::args().collect();
    if args.contains(&"--dbus".to_string()) {
        info!("Starting D-Bus service...");
        let _conn = ConnectionBuilder::session()?
            .name("org.gnome.RawThumbnailer")?
            .serve_at("/org/gnome/RawThumbnailer", Thumbnailer)?
            .build()
            .await?;

        // Keep the service running
        std::future::pending::<()>().await;
    } else {
        // Parse arguments manually to handle -s flag
        // Expected usage: raw-thumbnailer [-s size] <input> <output>
        let mut input_path_str = String::new();
        let mut output_path_str = String::new();
        let mut size = 512; // Default size

        let mut iter = args.iter().skip(1);
        while let Some(arg) = iter.next() {
            if arg == "-s" {
                if let Some(s) = iter.next() {
                    if let Ok(parsed_size) = s.parse::<u32>() {
                        size = parsed_size;
                    }
                }
            } else if input_path_str.is_empty() {
                input_path_str = arg.clone();
            } else if output_path_str.is_empty() {
                output_path_str = arg.clone();
            }
        }

        if input_path_str.is_empty() || output_path_str.is_empty() {
            error!("Usage: {} [-s size] <input.raw> <output.png>", args[0]);
            std::process::exit(1);
        }

        let input_path = Path::new(&input_path_str);
        let output_path = Path::new(&output_path_str);

        info!("Generating thumbnail for {:?} with size {}...", input_path, size);
        match generate_thumbnail(input_path, size) {
            Ok(thumbnail) => {
                info!("Saving thumbnail to {:?}...", output_path);
                thumbnail.save(output_path)?;
                info!("Thumbnail created successfully.");
            }
            Err(e) => {
                error!("Failed to generate thumbnail for {:?}: {}", input_path, e);
                return Err(e);
            }
        }
    }

    Ok(())
}

fn generate_thumbnail(path: &Path, size: u32) -> Result<ImageBuffer<Rgb<u8>, Vec<u8>>> {
    let params = RawDecodeParams { image_index: 0 };

    // First, try to extract the embedded preview, which is fastest.
    // Wrap in a scope to ensure RawSource is dropped before returning.
    if let Ok(preview) = {
        let result = extract_preview_pixels(path, &params);
        // Extract preview_pixels internally creates and drops RawSource
        result
    } {
        info!("Successfully extracted preview for {:?}", path);
        let thumbnail = preview.thumbnail(size, size);
        return Ok(thumbnail.to_rgb8());
    }

    info!(
        "No preview found or preview extraction failed, trying to decode preview image from {:?}",
        path
    );

    // Try to decode preview image, ensuring RawSource is dropped before returning
    let preview_result = {
        let raw_source = rawler::rawsource::RawSource::new(path)?;
        if let Ok(decoder) = rawler::get_decoder(&raw_source) {
            if let Ok(Some(preview)) = decoder.preview_image(&raw_source, &params) {
                info!("Successfully decoded preview image for {:?}", path);
                Some(preview)
            } else {
                None
            }
        } else {
            None
        }
        // raw_source is dropped here, closing all file handles
    };
    
    if let Some(preview) = preview_result {
        let thumbnail = preview.thumbnail(size, size);
        return Ok(thumbnail.to_rgb8());
    }

    // If preview extraction fails, fall back to decoding the full raw image.
    info!(
        "No preview found or preview extraction failed, decoding full image for {:?}",
        path
    );
    
    // Wrap full decode in a scope to ensure resources are dropped
    let image = {
        let raw_image = rawler::decode_file(path)?;
        let developed_image = RawDevelop::default().develop_intermediate(&raw_image)?;
        developed_image
            .to_dynamic_image()
            .ok_or_else(|| anyhow::anyhow!("Failed to convert to dynamic image"))?
        // raw_image is dropped here, closing all file handles
    };
    
    let thumbnail = image.thumbnail(size, size);
    Ok(thumbnail.to_rgb8())
}
