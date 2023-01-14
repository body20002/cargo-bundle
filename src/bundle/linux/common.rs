use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};
use image::{GenericImage, ImageDecoder};
use image::png::{PNGDecoder, PNGEncoder};
use libflate::gzip;
use md5::Digest;
use walkdir::WalkDir;
use bundle::{common, Settings};

/// Generate the application desktop file and store it under the `data_dir`.
pub fn generate_desktop_file(settings: &Settings, data_dir: &Path) -> ::Result<()> {
    let bin_name = settings.binary_name();
    let desktop_file_name = format!("{}.desktop", bin_name);
    let desktop_file_path = data_dir.join("usr/share/applications").join(desktop_file_name);
    let file = &mut common::create_file(&desktop_file_path)?;
    let mime_types = settings.linux_mime_types().iter().fold(
        "".to_owned(),
        |acc, s| format!("{}{};", acc, s),
    );
    // For more information about the format of this file, see
    // https://developer.gnome.org/integration-guide/stable/desktop-files.html.en
    write!(file, "[Desktop Entry]\n")?;
    write!(file, "Encoding=UTF-8\n")?;
    if let Some(category) = settings.app_category() {
        write!(file, "Categories={}\n", category.gnome_desktop_categories())?;
    }
    if !settings.short_description().is_empty() {
        write!(file, "Comment={}\n", settings.short_description())?;
    }
    let exec;
    match settings.linux_exec_args() {
        Some(args) => exec = format!("{} {}", bin_name, args),
        None => exec = bin_name.to_owned(),
    }
    write!(file, "Exec={}\n", exec)?;
    write!(file, "Icon={}\n", bin_name)?;
    write!(file, "Name={}\n", settings.bundle_name())?;
    write!(file, "Terminal={}\n", settings.linux_use_terminal().unwrap_or(false))?;
    write!(file, "Type=Application\n")?;
    write!(file, "MimeType={}\n", mime_types)?;
    // The `Version` field is omitted on pupose. See `generate_control_file` for specifying
    // the application version.
    Ok(())
}

/// Creates a `.tar.gz` file from the given directory (placing the new file
/// within the given directory's parent directory), then deletes the original
/// directory and returns the path to the new file.
pub fn tar_and_gzip_dir<P: AsRef<Path>>(src_dir: P) -> ::Result<PathBuf> {
    let src_dir = src_dir.as_ref();
    let dest_path = src_dir.with_extension("tar.gz");
    let dest_file = common::create_file(&dest_path)?;
    let gzip_encoder = gzip::Encoder::new(dest_file)?;
    let gzip_encoder = create_tar_from_dir(src_dir, gzip_encoder)?;
    let mut dest_file = gzip_encoder.finish().into_result()?;
    dest_file.flush()?;
    Ok(dest_path)
}

/// Writes a tar file to the given writer containing the given directory.
pub fn create_tar_from_dir<P: AsRef<Path>, W: Write>(src_dir: P, dest_file: W) -> ::Result<W> {
    let src_dir = src_dir.as_ref();
    let mut tar_builder = tar::Builder::new(dest_file);
    for entry in WalkDir::new(&src_dir) {
        let entry = entry?;
        let src_path = entry.path();
        if src_path == src_dir {
            continue;
        }
        let dest_path = src_path.strip_prefix(&src_dir).unwrap();
        if entry.file_type().is_dir() {
            tar_builder.append_dir(dest_path, src_path)?;
        } else {
            let mut src_file = std::fs::File::open(src_path)?;
            tar_builder.append_file(dest_path, &mut src_file)?;
        }
    }
    let dest_file = tar_builder.into_inner()?;
    Ok(dest_file)
}

/// Create an empty file at the given path, creating any parent directories as
/// needed, then write `data` into the file.
pub fn create_file_with_data<P: AsRef<Path>>(path: P, data: &str) -> ::Result<()> {
    let mut file = common::create_file(path.as_ref())?;
    file.write_all(data.as_bytes())?;
    file.flush()?;
    Ok(())
}

/// Computes the total size, in bytes, of the given directory and all of its
/// contents.
pub fn total_dir_size(dir: &Path) -> ::Result<u64> {
    let mut total: u64 = 0;
    for entry in WalkDir::new(&dir) {
        total += entry?
            .metadata()?
            .len();
    }
    Ok(total)
}

/// Generate the icon files and store them under the `data_dir`.
pub fn generate_icon_files(settings: &Settings, data_dir: &PathBuf) -> ::Result<()> {
    let base_dir = data_dir.join("usr/share/icons/hicolor");
    let get_dest_path = |width: u32, height: u32, is_high_density: bool| {
        base_dir.join(format!("{}x{}{}/apps/{}.png",
                              width,
                              height,
                              if is_high_density { "@2x" } else { "" },
                              settings.binary_name()))
    };
    let mut sizes = BTreeSet::new();
    // Prefer PNG files.
    for icon_path in settings.icon_files() {
        let icon_path = icon_path?;
        if icon_path.extension() != Some(OsStr::new("png")) {
            continue;
        }
        let mut decoder = PNGDecoder::new(File::open(&icon_path)?);
        let (width, height) = decoder.dimensions()?;
        let is_high_density = common::is_retina(&icon_path);
        if !sizes.contains(&(width, height, is_high_density)) {
            sizes.insert((width, height, is_high_density));
            let dest_path = get_dest_path(width, height, is_high_density);
            common::copy_file(&icon_path, &dest_path)?;
        }
    }
    // Fall back to non-PNG files for any missing sizes.
    for icon_path in settings.icon_files() {
        let icon_path = icon_path?;
        if icon_path.extension() == Some(OsStr::new("png")) {
            continue;
        } else if icon_path.extension() == Some(OsStr::new("icns")) {
            let icon_family = icns::IconFamily::read(File::open(&icon_path)?)?;
            for icon_type in icon_family.available_icons() {
                let width = icon_type.screen_width();
                let height = icon_type.screen_height();
                let is_high_density = icon_type.pixel_density() > 1;
                if !sizes.contains(&(width, height, is_high_density)) {
                    sizes.insert((width, height, is_high_density));
                    let dest_path = get_dest_path(width, height, is_high_density);
                    let icon = icon_family.get_icon_with_type(icon_type)?;
                    icon.write_png(common::create_file(&dest_path)?)?;
                }
            }
        } else {
            let icon = image::open(&icon_path)?;
            let (width, height) = icon.dimensions();
            let is_high_density = common::is_retina(&icon_path);
            if !sizes.contains(&(width, height, is_high_density)) {
                sizes.insert((width, height, is_high_density));
                let dest_path = get_dest_path(width, height, is_high_density);
                let encoder = PNGEncoder::new(common::create_file(&dest_path)?);
                encoder.encode(&icon.raw_pixels(), width, height, icon.color())?;
            }
        }
    }
    Ok(())
}

/// Compute the md5 hash of the given file.
pub fn generate_md5sum(file_path: &Path) -> ::Result<Digest> {
    let mut file = File::open(file_path)?;
    let mut hash = md5::Context::new();
    io::copy(&mut file, &mut hash)?;
    Ok(hash.compute())
}

#[cfg(test)]
mod tests {
    use super::*;
    use  tempfile::tempdir;
    use std::assert_matches::assert_matches;
    use std::fs::File;
    use std::io::{Write, Read};

    #[test]
    fn test_generate_desktop_file() {
        let settings = Settings::default();
        let  temp_dir = tempdir().unwrap();
        println!("temp_dir: {:?}", temp_dir);
        assert_matches!(generate_desktop_file(&settings, temp_dir.path()), Ok(()));

        let desktop_file_contents = {
            let mut file = File::open(temp_dir.path()
                .join("usr/share/applications/app.desktop")).unwrap();
            let mut contents = String::new();
            file.read_to_string(&mut contents).unwrap();
            contents
        };
        assert_eq!(desktop_file_contents, "[Desktop Entry]\nEncoding=UTF-8\nComment=No description\n\
        Exec=app\nIcon=app\nName=app\nTerminal=false\nType=Application\n\
        MimeType=application/x-app;\n");
    }

    #[test]
    fn test_tar_and_gzip_dir() {
        let temp_dir = tempdir().unwrap();
        std::fs::create_dir(temp_dir.path().join("foo")).unwrap();
        File::create(temp_dir.path().join("foo/file1.txt")).unwrap();
        std::fs::create_dir_all(temp_dir.path().join("foo/subdir")).unwrap();
        File::create(temp_dir.path().join("foo/subdir/file2.txt")).unwrap()
            .write_all(b"test").unwrap();
        let  tar_gz_file = tar_and_gzip_dir(&temp_dir.path().join("foo"));
        assert_matches!(tar_gz_file, Ok(_));
        let tar_gz_file = tar_gz_file.unwrap();

        assert!(tar_gz_file.exists());
        assert!(tar_gz_file.metadata().unwrap().len() > 0);
    }

    #[test]
    fn test_create_file_with_data() {
        let temp_dir = tempdir().unwrap();
        let file_path = temp_dir.path().join("foo.txt");
        assert_matches!(create_file_with_data(&file_path, "test"), Ok(()));
        assert!(file_path.exists());
        assert_eq!(file_path.metadata().unwrap().len(), 4);
    }

    #[test]
    fn test_total_dir_size() {
        let temp_dir = tempdir().unwrap();
        File::create(temp_dir.path().join("file1.txt")).unwrap()
            .write_all(b"test").unwrap();
        std::fs::create_dir_all(temp_dir.path().join("subdir")).unwrap();
        File::create(temp_dir.path().join("subdir/file2.txt")).unwrap()
            .write_all(b"test").unwrap();
        assert_matches!(total_dir_size(temp_dir.path()), Ok(148));
    }

    #[test]
    fn test_generate_md5sum() {
        let temp_dir = tempdir().unwrap();
        let file_path = temp_dir.path().join("foo.txt");
        File::create(&file_path).unwrap().write_all(b"test").unwrap();
        let  md5_sums = generate_md5sum(file_path.as_path());
        assert_matches!(md5_sums, Ok(_));
        let mut md5_str = String::new();

        for b in md5_sums.unwrap().iter() {
            md5_str.push_str(&format!("{:02x}", b));
        }

        assert_eq!(md5_str, "098f6bcd4621d373cade4e832627b4f6".to_string());
    }
}