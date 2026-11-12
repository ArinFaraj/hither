pub const MAX_IMAGE: u64 = 40 << 20;

#[derive(Clone, Debug, PartialEq)]
pub enum Clip {
    Text(String),
    Image(Vec<u8>),
}

impl Clip {
    pub fn is_empty(&self) -> bool {
        match self {
            Clip::Text(t) => t.is_empty(),
            Clip::Image(b) => b.is_empty(),
        }
    }

    pub fn hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        match self {
            Clip::Text(t) => t.hash(&mut h),
            Clip::Image(b) => {
                1u8.hash(&mut h);
                b.hash(&mut h);
            }
        }
        h.finish()
    }

    pub fn describe(&self) -> String {
        match self {
            Clip::Text(t) => format!("{} chars", t.chars().count()),
            Clip::Image(b) => format!("image, {}", crate::localsend::human(b.len() as u64)),
        }
    }
}

pub fn is_png(b: &[u8]) -> bool {
    b.starts_with(b"\x89PNG\r\n\x1a\n")
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{Clip, MAX_IMAGE};
    use objc2::rc::autoreleasepool;
    use objc2_app_kit::{
        NSBitmapImageFileType, NSBitmapImageRep, NSPasteboard, NSPasteboardTypePNG, NSPasteboardTypeString, NSPasteboardTypeTIFF,
    };
    use objc2_foundation::{NSData, NSDictionary, NSString};

    pub fn available() -> bool {
        true
    }

    pub fn change_count() -> i64 {
        autoreleasepool(|_| NSPasteboard::generalPasteboard().changeCount() as i64)
    }

    #[allow(unused_unsafe)]
    pub fn get() -> Option<Clip> {
        autoreleasepool(|_| {
            let pb = NSPasteboard::generalPasteboard();
            if let Some(s) = unsafe { pb.stringForType(NSPasteboardTypeString) } {
                return Some(Clip::Text(s.to_string()));
            }
            if let Some(d) = unsafe { pb.dataForType(NSPasteboardTypePNG) } {
                return (d.length() as u64 <= MAX_IMAGE).then(|| Clip::Image(d.to_vec()));
            }
            let d = unsafe { pb.dataForType(NSPasteboardTypeTIFF) }?;
            if d.length() as u64 > 4 * MAX_IMAGE {
                return None;
            }
            png_from(&d).filter(|p| p.len() as u64 <= MAX_IMAGE).map(Clip::Image)
        })
    }

    #[allow(unused_unsafe)]
    fn png_from(data: &NSData) -> Option<Vec<u8>> {
        let rep = NSBitmapImageRep::imageRepWithData(data)?;
        let props = NSDictionary::new();
        unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &props) }.map(|d| d.to_vec())
    }

    pub fn to_png(bytes: &[u8]) -> Option<Vec<u8>> {
        if super::is_png(bytes) {
            return Some(bytes.to_vec());
        }
        autoreleasepool(|_| png_from(&NSData::with_bytes(bytes)))
    }

    #[allow(unused_unsafe)]
    pub fn set(clip: &Clip) {
        autoreleasepool(|_| {
            let pb = NSPasteboard::generalPasteboard();
            pb.clearContents();
            match clip {
                Clip::Text(t) => unsafe {
                    pb.setString_forType(&NSString::from_str(t), NSPasteboardTypeString);
                },
                Clip::Image(b) => unsafe {
                    pb.setData_forType(Some(&NSData::with_bytes(b)), NSPasteboardTypePNG);
                },
            }
        })
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::Clip;

    pub fn available() -> bool {
        false
    }

    pub fn change_count() -> i64 {
        0
    }

    pub fn get() -> Option<Clip> {
        None
    }

    pub fn to_png(bytes: &[u8]) -> Option<Vec<u8>> {
        super::is_png(bytes).then(|| bytes.to_vec())
    }

    pub fn set(_clip: &Clip) {}
}

pub use imp::*;
