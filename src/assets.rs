//! Images pasted into a block, stored the way Logseq stores them: the file is
//! copied into `<root>/assets/` as `<stem>_<unix millis>_<n>.<ext>` and the
//! block gets `![<stem>.<ext>](../assets/<file>)`. The link is relative to the
//! note's file; `pages/` and `journals/` both sit one level below the root, so
//! the same link works from either.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use chrono::Utc;
use gpui::{App, ClipboardEntry, ClipboardItem, Image, ImageFormat, Img};

use crate::errors;
use crate::registry::PageRegistry;

/// One image a paste carries.
pub enum PastedImage {
    /// Raw image data, such as a screenshot.
    Data(Image),
    /// An image file copied in a file manager.
    File(PathBuf),
}

/// The images on the clipboard, or none when the paste should go in as text.
/// Copied files that `img` can't display are left out, so copying only a PDF
/// still pastes its path. Image data counts only without text beside it: an
/// app that offers both (a spreadsheet's cells) means the text.
#[must_use]
pub fn clipboard_images(item: &ClipboardItem) -> Vec<PastedImage> {
    let files: Vec<PastedImage> = item
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            ClipboardEntry::ExternalPaths(paths) => Some(paths.paths()),
            _ => None,
        })
        .flatten()
        .filter(|path| is_displayable(path))
        .map(|path| PastedImage::File(path.clone()))
        .collect();
    if !files.is_empty() {
        return files;
    }
    let has_text = item
        .entries()
        .iter()
        .any(|entry| matches!(entry, ClipboardEntry::String(_)));
    if has_text {
        return Vec::new();
    }
    item.entries()
        .iter()
        .filter_map(|entry| match entry {
            ClipboardEntry::Image(image) => Some(PastedImage::Data(image.clone())),
            _ => None,
        })
        .collect()
}

/// What a paste turned out to be, for the paste handlers.
pub enum ImagePaste {
    /// Not images: paste as before.
    NotImages,
    /// Images that couldn't be saved; the error is already reported.
    Failed,
    /// The saved images' markdown links.
    Saved(String),
}

/// Saves the clipboard's images into the open graph's `assets/`.
pub fn paste_from_clipboard(cx: &mut App) -> ImagePaste {
    let Some(item) = cx.read_from_clipboard() else {
        return ImagePaste::NotImages;
    };
    let images = clipboard_images(&item);
    let Some(registry) = cx.try_global::<PageRegistry>() else {
        return ImagePaste::NotImages;
    };
    if images.is_empty() {
        return ImagePaste::NotImages;
    }
    match save(registry.root(), &images, Utc::now().timestamp_millis()) {
        Ok(links) => ImagePaste::Saved(links),
        Err(err) => {
            errors::report(format!("Couldn't save pasted image: {err}"), cx);
            ImagePaste::Failed
        }
    }
}

/// Copies each image into `<root>/assets/` and returns their markdown links,
/// one per line. Never overwrites: a taken name moves on to the next index.
///
/// # Errors
/// Returns an error if the assets directory or a file can't be written, or a
/// copied file can't be read.
pub fn save(root: &Path, images: &[PastedImage], millis: i64) -> io::Result<String> {
    let dir = root.join("assets");
    fs::create_dir_all(&dir)?;
    let mut links = Vec::new();
    for image in images {
        let (stem, ext) = match image {
            PastedImage::Data(image) => ("image".to_string(), extension(image.format).to_string()),
            PastedImage::File(path) => (
                sanitize(&path.file_stem().unwrap_or_default().to_string_lossy()),
                path.extension()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase(),
            ),
        };
        let (name, mut file) = create_unique(&dir, &stem, &ext, millis)?;
        let written = match image {
            PastedImage::Data(image) => file.write_all(&image.bytes),
            PastedImage::File(path) => File::open(path).and_then(|mut src| {
                io::copy(&mut src, &mut file)?;
                Ok(())
            }),
        };
        if let Err(err) = written {
            // Don't leave a truncated file behind for nothing to link to.
            let _ = fs::remove_file(dir.join(&name));
            return Err(err);
        }
        links.push(format!("![{stem}.{ext}](../assets/{name})"));
    }
    Ok(links.join("\n"))
}

/// Whether a markdown image link points at a local file `img` can display.
/// Anything else (a PDF, a web URL) keeps rendering as its alt text.
#[must_use]
pub fn is_image_link(url: &str) -> bool {
    !url.contains("://") && is_displayable(Path::new(url))
}

/// The file an image link names. Relative links resolve against `pages/`,
/// which stands in for `journals/`: both are one level below the root.
#[must_use]
pub fn resolve(root: &Path, url: &str) -> PathBuf {
    root.join("pages").join(url)
}

fn is_displayable(path: &Path) -> bool {
    path.extension().is_some_and(|ext| {
        let ext = ext.to_string_lossy().to_lowercase();
        Img::extensions().contains(&ext.as_str())
    })
}

fn extension(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpg",
        ImageFormat::Webp => "webp",
        ImageFormat::Gif => "gif",
        ImageFormat::Svg => "svg",
        ImageFormat::Bmp => "bmp",
        ImageFormat::Tiff => "tiff",
        ImageFormat::Ico => "ico",
    }
}

/// A file stem safe inside a markdown link: anything but letters, digits,
/// `-` and `_` (spaces, brackets, parentheses) becomes `_`.
fn sanitize(stem: &str) -> String {
    let clean: String = stem
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if clean.is_empty() {
        "image".to_string()
    } else {
        clean
    }
}

fn create_unique(dir: &Path, stem: &str, ext: &str, millis: i64) -> io::Result<(String, File)> {
    for index in 0.. {
        let name = format!("{stem}_{millis}_{index}.{ext}");
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join(&name))
        {
            Ok(file) => return Ok((name, file)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }
    unreachable!("an unbounded range always yields a free index")
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::ExternalPaths;
    use rstest::rstest;

    fn png(bytes: &[u8]) -> Image {
        Image::from_bytes(ImageFormat::Png, bytes.to_vec())
    }

    #[test]
    fn a_screenshot_is_saved_with_a_logseq_name_and_link() {
        let root = tempfile::tempdir().unwrap();
        let link = save(root.path(), &[PastedImage::Data(png(b"px"))], 1703112558521).unwrap();
        assert_eq!(link, "![image.png](../assets/image_1703112558521_0.png)");
        let saved = root.path().join("assets/image_1703112558521_0.png");
        assert_eq!(fs::read(saved).unwrap(), b"px");
    }

    #[test]
    fn a_taken_name_moves_to_the_next_index() {
        let root = tempfile::tempdir().unwrap();
        let images = [PastedImage::Data(png(b"a")), PastedImage::Data(png(b"b"))];
        let links = save(root.path(), &images, 5).unwrap();
        assert_eq!(
            links,
            "![image.png](../assets/image_5_0.png)\n![image.png](../assets/image_5_1.png)"
        );
        let first = save(root.path(), &[PastedImage::Data(png(b"c"))], 5).unwrap();
        assert_eq!(first, "![image.png](../assets/image_5_2.png)");
        assert_eq!(fs::read(root.path().join("assets/image_5_0.png")).unwrap(), b"a");
    }

    #[test]
    fn a_copied_file_keeps_its_name_made_link_safe() {
        let root = tempfile::tempdir().unwrap();
        let src = root.path().join("My Shot (2).PNG");
        fs::write(&src, b"file").unwrap();
        let link = save(root.path(), &[PastedImage::File(src)], 7).unwrap();
        assert_eq!(link, "![My_Shot__2_.png](../assets/My_Shot__2__7_0.png)");
        assert_eq!(
            fs::read(root.path().join("assets/My_Shot__2__7_0.png")).unwrap(),
            b"file"
        );
    }

    #[test]
    fn a_failed_copy_leaves_no_file_behind() {
        let root = tempfile::tempdir().unwrap();
        let missing = PastedImage::File(root.path().join("gone.png"));
        assert!(save(root.path(), &[missing], 7).is_err());
        assert_eq!(fs::read_dir(root.path().join("assets")).unwrap().count(), 0);
    }

    fn files(paths: &[&str]) -> ClipboardEntry {
        ClipboardEntry::ExternalPaths(ExternalPaths(
            paths.iter().map(PathBuf::from).collect(),
        ))
    }

    #[test]
    fn only_displayable_files_count() {
        let item = ClipboardItem {
            entries: vec![files(&["/a/doc.pdf", "/a/shot.JPG"]), "doc.pdf".to_string().into()],
        };
        let images = clipboard_images(&item);
        assert!(matches!(&images[..], [PastedImage::File(p)] if p.ends_with("shot.JPG")));

        let pdf_only = ClipboardItem {
            entries: vec![files(&["/a/doc.pdf"])],
        };
        assert!(clipboard_images(&pdf_only).is_empty(), "pastes as text");
    }

    #[test]
    fn image_data_with_text_beside_it_pastes_as_text() {
        assert_eq!(clipboard_images(&ClipboardItem::new_image(&png(b"x"))).len(), 1);
        let both = ClipboardItem {
            entries: vec!["cells".to_string().into(), png(b"x").into()],
        };
        assert!(clipboard_images(&both).is_empty());
    }

    #[rstest]
    #[case("../assets/image_1_0.png", true)]
    #[case("../assets/photo.JPEG", true)]
    #[case("../assets/drawing.svg", true)]
    #[case("../assets/nobbs_1_0.pdf", false)]
    #[case("https://example.com/cat.png", false)]
    #[case("../assets/noext", false)]
    fn image_links(#[case] url: &str, #[case] expected: bool) {
        assert_eq!(is_image_link(url), expected);
    }
}
