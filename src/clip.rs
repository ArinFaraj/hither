#[cfg(target_os = "macos")]
mod imp {
    use objc2::rc::autoreleasepool;
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
    use objc2_foundation::NSString;

    pub fn available() -> bool {
        true
    }

    pub fn change_count() -> i64 {
        autoreleasepool(|_| NSPasteboard::generalPasteboard().changeCount() as i64)
    }

    #[allow(unused_unsafe)]
    pub fn get() -> Option<String> {
        autoreleasepool(|_| {
            let pb = NSPasteboard::generalPasteboard();
            unsafe { pb.stringForType(NSPasteboardTypeString) }.map(|s| s.to_string())
        })
    }

    #[allow(unused_unsafe)]
    pub fn set(text: &str) {
        autoreleasepool(|_| {
            let pb = NSPasteboard::generalPasteboard();
            pb.clearContents();
            unsafe { pb.setString_forType(&NSString::from_str(text), NSPasteboardTypeString) };
        })
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub fn available() -> bool {
        false
    }

    pub fn change_count() -> i64 {
        0
    }

    pub fn get() -> Option<String> {
        None
    }

    pub fn set(_text: &str) {}
}

pub use imp::*;

pub fn hash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}
