use anyhow::Result;
use image::{ImageBuffer, Rgb};
use log::{error, info};
use rawler::analyze::extract_preview_pixels;
use rawler::decoders::RawDecodeParams;
use rawler::imgop::develop::RawDevelop;
use simplelog::{Config, LevelFilter, WriteLogger};
use std::env;
use std::fs::OpenOptions;
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

        match generate_thumbnail(input_path) {
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
    if let Ok(log_file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/raw-thumbnailer.log")
    {
        // The service might fail to initialize if another instance is running,
        // but we don't have a logger to report it to. We'll just ignore it.
        let _ = WriteLogger::init(LevelFilter::Info, Config::default(), log_file);
    }

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
        // Original command-line functionality for testing
        if args.len() != 3 {
            error!("Usage: {} <input.raw> <output.png>", args[0]);
            std::process::exit(1);
        }

        let input_path = Path::new(&args[1]);
        let output_path = Path::new(&args[2]);

        info!("Generating thumbnail for {:?}...", input_path);
        match generate_thumbnail(input_path) {
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

fn generate_thumbnail(path: &Path) -> Result<ImageBuffer<Rgb<u8>, Vec<u8>>> {
    let params = RawDecodeParams { image_index: 0 };

    // First, try to extract the embedded preview, which is fastest.
    // Wrap in a scope to ensure RawSource is dropped before returning.
    if let Ok(preview) = {
        let result = extract_preview_pixels(path, &params);
        // Extract preview_pixels internally creates and drops RawSource
        result
    } {
        info!("Successfully extracted preview for {:?}", path);
        let thumbnail = preview.thumbnail(512, 512);
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
        let thumbnail = preview.thumbnail(512, 512);
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
    
    let thumbnail = image.thumbnail(512, 512);
    Ok(thumbnail.to_rgb8())
}
