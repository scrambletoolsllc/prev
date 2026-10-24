//! Makes prev the system's default app for every file type it opens. This
//! changes only the system's setting, never prev's own.
//!
//! - Linux: the `[Default Applications]` of `mimeapps.list` in the user's
//!   config folder, as `xdg-mime default` writes it, for every MIME type
//!   the desktop entry lists.
//! - macOS: Launch Services' default handler for the common types, PDF
//!   and the usual photo formats. macOS asks the user to confirm each one,
//!   so the rarer types are left to Finder's Get Info.
//! - Windows lets only the user choose default apps, in Settings; this
//!   opens prev's page there.
//!
//! `status` tells how many of those types prev opens now, and
//! `move_old_defaults` hands this prev the defaults given to prev under
//! its old app ID, `io.github.scrambletools.prev`.

/// What changing the default did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// prev is now the default for this many file types.
    Set(usize),
    /// The system's own page for choosing opened, where the user chooses.
    OpenedSettings,
    /// macOS: the change was asked for; the system asks the user to
    /// confirm each type.
    Asked,
}

/// Why prev could not become the default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Linux: the desktop entry is not installed, so the system has
    /// nothing to open files with.
    NoDesktopEntry,
    /// macOS: prev is not running from prev.app.
    NoBundle,
    Other(String),
}

/// How many of the file types the button covers prev opens now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    pub set: usize,
    pub total: usize,
}

/// Makes prev the default app. Blocks briefly; run it off the interface
/// thread.
pub async fn make_default() -> Result<Outcome, Failure> {
    platform::make_default().await
}

/// How many of the file types prev opens now; `None` when that cannot be
/// told, such as for a Mac build not run from prev.app.
pub async fn status() -> Option<Status> {
    platform::status()
}

/// Makes this prev the default wherever prev under its old app ID was,
/// once that one is gone, so a choice made before the ID changed carries
/// over. Blocks briefly; run it off the interface thread.
pub async fn move_old_defaults() {
    platform::move_old_defaults().await;
}

#[cfg(target_os = "linux")]
mod platform {
    use std::path::{Path, PathBuf};

    use super::{Failure, Outcome, Status};

    /// The desktop entry every prev package and install script installs,
    /// or inside the Flatpak the one it exports, named for the Flatpak.
    fn desktop_id() -> String {
        match std::env::var("FLATPAK_ID") {
            Ok(id) if !id.is_empty() => format!("{id}.desktop"),
            _ => "run.prev.desktop".to_owned(),
        }
    }
    const SECTION: &str = "[Default Applications]";
    /// The desktop entry of prev before its app ID became run.prev, the
    /// Flatpak's too.
    const OLD_ID: &str = "io.github.scrambletools.prev.desktop";

    pub async fn make_default() -> Result<Outcome, Failure> {
        let flatpak = std::env::var_os("FLATPAK_ID").is_some();
        // Inside the Flatpak the desktop entry is exported outside the
        // sandbox, where prev cannot look.
        if !flatpak && !entry_installed(&desktop_id()) {
            return Err(Failure::NoDesktopEntry);
        }
        let types: Vec<&str> = crate::filetype::supported_mime_types().collect();
        for list in lists(flatpak) {
            let text = match std::fs::read_to_string(&list) {
                Ok(text) => text,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(error) => return Err(Failure::Other(error.to_string())),
            };
            let updated = set_defaults(&text, &types, &desktop_id());
            if updated != text {
                if let Some(parent) = list.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|error| Failure::Other(error.to_string()))?;
                }
                prev_store::atomic::write(&list, updated.as_bytes())
                    .map_err(|error| Failure::Other(error.to_string()))?;
            }
        }
        Ok(Outcome::Set(types.len()))
    }

    pub fn status() -> Option<Status> {
        let flatpak = std::env::var_os("FLATPAK_ID").is_some();
        let lists = lookup_order(flatpak);
        let texts: Vec<String> = lists
            .iter()
            .filter_map(|path| std::fs::read_to_string(path).ok())
            .collect();
        let types: Vec<&str> = crate::filetype::supported_mime_types().collect();
        let ours = desktop_id();
        let set = types
            .iter()
            .filter(|mime| {
                texts
                    .iter()
                    .find_map(|text| default_in(text, mime))
                    .is_some_and(|desktop| desktop == ours)
            })
            .count();
        Some(Status {
            set,
            total: types.len(),
        })
    }

    /// Renames the old desktop entry to this prev's in the user's lists,
    /// once this prev's entry is installed and the old one is gone: a
    /// default then never names an entry that is not there, and a prev
    /// still installed under the old ID keeps what was chosen for it.
    pub async fn move_old_defaults() {
        let flatpak = std::env::var_os("FLATPAK_ID").is_some();
        if (!flatpak && !entry_installed(&desktop_id())) || entry_installed(OLD_ID) {
            return;
        }
        for list in lists(flatpak) {
            let Ok(text) = std::fs::read_to_string(&list) else {
                continue;
            };
            let updated = rename_entry(&text, OLD_ID, &desktop_id());
            if updated != text
                && let Err(error) = prev_store::atomic::write(&list, updated.as_bytes())
            {
                eprintln!("prev: could not update {}: {error}", list.display());
            }
        }
    }

    /// The files the system reads defaults from, first one first: each
    /// config and data folder's desktop-specific list, then its
    /// `mimeapps.list`, then the older `defaults.list` in data folders.
    fn lookup_order(flatpak: bool) -> Vec<PathBuf> {
        let home = prev_store::paths::home().unwrap_or_default();
        let desktops: Vec<String> = std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .split(':')
            .filter(|name| !name.is_empty())
            .map(str::to_lowercase)
            .collect();
        let dirs = |variable: &str, fallback: &str| -> Vec<PathBuf> {
            std::env::var(variable)
                .ok()
                .filter(|dirs| !dirs.is_empty())
                .unwrap_or_else(|| fallback.into())
                .split(':')
                .map(PathBuf::from)
                .collect()
        };
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| home.join(".local/share"));
        let mut config = vec![config_home(flatpak)];
        config.extend(dirs("XDG_CONFIG_DIRS", "/etc/xdg"));
        let mut data = vec![data_home];
        data.extend(dirs("XDG_DATA_DIRS", "/usr/local/share:/usr/share"));
        let mut order = Vec::new();
        for dir in config
            .into_iter()
            .chain(data.iter().map(|dir| dir.join("applications")))
        {
            for desktop in &desktops {
                order.push(dir.join(format!("{desktop}-mimeapps.list")));
            }
            order.push(dir.join("mimeapps.list"));
        }
        order.extend(
            data.iter()
                .map(|dir| dir.join("applications/defaults.list")),
        );
        order
    }

    /// The default app `text`, a `mimeapps.list`, names for `mime`: the
    /// first in its list under `[Default Applications]`.
    pub fn default_in(text: &str, mime: &str) -> Option<String> {
        let mut in_section = false;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_section = line == SECTION;
                continue;
            }
            if !in_section {
                continue;
            }
            if let Some((key, value)) = line.split_once('=')
                && key.trim() == mime
            {
                return value
                    .split(';')
                    .map(str::trim)
                    .find(|desktop| !desktop.is_empty())
                    .map(str::to_owned);
            }
        }
        None
    }

    /// The user's `mimeapps.list`, and the desktop's own one beside it
    /// when there is one, such as `hyprland-mimeapps.list`, which the
    /// system reads first.
    fn lists(flatpak: bool) -> Vec<PathBuf> {
        let config = config_home(flatpak);
        let mut lists = vec![config.join("mimeapps.list")];
        if let Ok(desktops) = std::env::var("XDG_CURRENT_DESKTOP") {
            for desktop in desktops.split(':').filter(|name| !name.is_empty()) {
                let own = config.join(format!("{}-mimeapps.list", desktop.to_lowercase()));
                if own.exists() {
                    lists.push(own);
                }
            }
        }
        lists
    }

    /// `$XDG_CONFIG_HOME`, or `~/.config`. The Flatpak's own points into
    /// its sandbox, so there it is always the home folder's.
    fn config_home(flatpak: bool) -> PathBuf {
        let home = prev_store::paths::home().unwrap_or_else(|| PathBuf::from("/"));
        if flatpak {
            return home.join(".config");
        }
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| home.join(".config"))
    }

    /// Whether a desktop entry named `id` is installed: in the data
    /// folders, and in `~/.local/share` and Flatpak's exports, which
    /// launchers read even where the variables leave them out, and which
    /// the Flatpak sees through its access to the home folder.
    fn entry_installed(id: &str) -> bool {
        let home = prev_store::paths::home().unwrap_or_default();
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| home.join(".local/share"));
        let data_dirs = std::env::var("XDG_DATA_DIRS")
            .ok()
            .filter(|dirs| !dirs.is_empty())
            .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        [
            data_home,
            home.join(".local/share"),
            home.join(".local/share/flatpak/exports/share"),
            PathBuf::from("/var/lib/flatpak/exports/share"),
        ]
        .into_iter()
        .chain(data_dirs.split(':').map(PathBuf::from))
        .any(|dir| Path::new(&dir).join("applications").join(id).exists())
    }

    /// `text`, a `mimeapps.list`, with the desktop entry `old` renamed
    /// `new` in every section, and named only once where a list already
    /// had `new`. Lines without `old` stay as they were.
    pub fn rename_entry(text: &str, old: &str, new: &str) -> String {
        let mut out = String::with_capacity(text.len());
        for line in text.split_inclusive('\n') {
            let (body, end) = match line.strip_suffix('\n') {
                Some(body) => (body, "\n"),
                None => (line, ""),
            };
            match body.split_once('=') {
                Some((key, value))
                    if !body.trim_start().starts_with(['#', '['])
                        && value.split(';').any(|desktop| desktop.trim() == old) =>
                {
                    let mut desktops: Vec<&str> = Vec::new();
                    for desktop in value.split(';').map(str::trim) {
                        let desktop = if desktop == old { new } else { desktop };
                        if !desktop.is_empty() && !desktops.contains(&desktop) {
                            desktops.push(desktop);
                        }
                    }
                    out.push_str(key);
                    out.push('=');
                    out.push_str(&desktops.join(";"));
                    if value.trim_end().ends_with(';') {
                        out.push(';');
                    }
                }
                _ => out.push_str(body),
            }
            out.push_str(end);
        }
        out
    }

    /// `text`, a `mimeapps.list`, with `desktop` the default for each of
    /// `types`: existing lines for them replaced, missing ones added to
    /// `[Default Applications]`, which is made when missing. Everything
    /// else stays as it was.
    pub fn set_defaults(text: &str, types: &[&str], desktop: &str) -> String {
        let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
        let start = match lines.iter().position(|line| line.trim() == SECTION) {
            Some(start) => start,
            None => {
                if lines.last().is_some_and(|line| !line.trim().is_empty()) {
                    lines.push(String::new());
                }
                lines.push(SECTION.to_owned());
                lines.len() - 1
            }
        };
        let end = lines[start + 1..]
            .iter()
            .position(|line| line.trim_start().starts_with('['))
            .map_or(lines.len(), |offset| start + 1 + offset);
        let mut section: Vec<String> = lines[start + 1..end]
            .iter()
            .filter(|line| {
                let key = line.split('=').next().unwrap_or("").trim();
                !types.contains(&key)
            })
            .cloned()
            .collect();
        // Keep a blank line before the next section, if there was one.
        let trailing_blank = section.last().is_some_and(|line| line.trim().is_empty());
        if trailing_blank {
            section.pop();
        }
        section.extend(types.iter().map(|mime| format!("{mime}={desktop}")));
        if trailing_blank || end < lines.len() {
            section.push(String::new());
        }
        lines.splice(start + 1..end, section);
        let mut out = lines.join("\n");
        out.push('\n');
        out
    }
}

#[cfg(target_os = "macos")]
mod platform {
    #![allow(unsafe_code)]

    use std::ffi::c_void;

    use objc2::rc::Retained;
    use objc2_foundation::{NSBundle, NSString};

    use super::{Failure, Outcome, Status};

    type CFStringRef = *const c_void;

    #[link(name = "CoreServices", kind = "framework")]
    unsafe extern "C" {
        fn LSSetDefaultRoleHandlerForContentType(
            content_type: CFStringRef,
            role: u32,
            bundle_id: CFStringRef,
        ) -> i32;
        fn LSCopyDefaultRoleHandlerForContentType(
            content_type: CFStringRef,
            role: u32,
        ) -> CFStringRef;
        fn LSCopyApplicationURLsForBundleIdentifier(
            bundle_id: CFStringRef,
            error: *mut *const c_void,
        ) -> *const c_void;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(object: *const c_void);
        fn CFArrayGetCount(array: *const c_void) -> isize;
    }

    /// The bundle identifier prev's began with before its app ID became
    /// run.prev, and the one it begins with now.
    const OLD_PREFIX: &str = "io.github.scrambletools.prev";
    const NEW_PREFIX: &str = "run.prev";

    /// Every role: viewing and editing.
    const ROLES_ALL: u32 = 0xFFFF_FFFF;

    /// The content types the button asks for: PDF and the usual photo
    /// formats. macOS asks the user about each, so the list stays short.
    pub const TYPES: &[&str] = &[
        "com.adobe.pdf",
        "public.png",
        "public.jpeg",
        "public.heic",
        "com.compuserve.gif",
        "public.tiff",
        "org.webmproject.webp",
        "public.avif",
    ];

    fn bundle() -> Option<Retained<NSString>> {
        NSBundle::mainBundle().bundleIdentifier()
    }

    pub async fn make_default() -> Result<Outcome, Failure> {
        let Some(bundle) = bundle() else {
            return Err(Failure::NoBundle);
        };
        let mut asked = 0;
        let mut last_error = None;
        for identifier in TYPES {
            let status = set_handler(identifier, &bundle);
            if status == 0 {
                asked += 1;
            } else {
                last_error = Some(status);
            }
        }
        match (asked, last_error) {
            (0, Some(status)) => Err(Failure::Other(format!("Launch Services error {status}"))),
            _ => Ok(Outcome::Asked),
        }
    }

    pub fn status() -> Option<Status> {
        let bundle = bundle()?.to_string();
        let set = TYPES
            .iter()
            .filter(|identifier| {
                handler(identifier).is_some_and(|name| name.eq_ignore_ascii_case(&bundle))
            })
            .count();
        Some(Status {
            set,
            total: TYPES.len(),
        })
    }

    /// Asks for this prev as the default for the types prev under its old
    /// bundle identifier opened, once that app is gone. macOS asks the user
    /// to confirm each, so this asks only once: a marker in prev's state
    /// folder keeps a user who said no from being asked again.
    pub async fn move_old_defaults() {
        let Some(bundle) = bundle() else {
            return;
        };
        let Some(old) = bundle
            .to_string()
            .strip_prefix(NEW_PREFIX)
            .map(|rest| format!("{OLD_PREFIX}{rest}"))
        else {
            return;
        };
        let Some(marker) = prev_store::paths::state_dir().map(|dir| dir.join("default-app-moved"))
        else {
            return;
        };
        if marker.exists() || installed(&old) {
            return;
        }
        for identifier in TYPES {
            if handler(identifier).is_some_and(|name| name.eq_ignore_ascii_case(&old)) {
                set_handler(identifier, &bundle);
            }
        }
        if let Some(parent) = marker.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&marker, "");
    }

    /// The bundle identifier of the default app for a content type.
    fn handler(identifier: &str) -> Option<String> {
        let content_type = NSString::from_str(identifier);
        // SAFETY: a live NSString is a CFString; the result is owned,
        // copied, then released.
        let handler = unsafe {
            LSCopyDefaultRoleHandlerForContentType((&raw const *content_type).cast(), ROLES_ALL)
        };
        if handler.is_null() {
            return None;
        }
        let name = unsafe { &*handler.cast::<NSString>() }.to_string();
        unsafe { CFRelease(handler) };
        Some(name)
    }

    /// Makes `bundle` the default app for a content type, which macOS asks
    /// the user to confirm; Launch Services' status, 0 when asked.
    fn set_handler(identifier: &str, bundle: &NSString) -> i32 {
        let content_type = NSString::from_str(identifier);
        // SAFETY: both strings are live NSStrings, which are CFStrings.
        unsafe {
            LSSetDefaultRoleHandlerForContentType(
                (&raw const *content_type).cast(),
                ROLES_ALL,
                (&raw const *bundle).cast(),
            )
        }
    }

    /// Whether any app with this bundle identifier is installed.
    fn installed(bundle: &str) -> bool {
        let bundle = NSString::from_str(bundle);
        // SAFETY: a live NSString is a CFString; the array, when there is
        // one, is owned, counted, then released.
        unsafe {
            let urls = LSCopyApplicationURLsForBundleIdentifier(
                (&raw const *bundle).cast(),
                std::ptr::null_mut(),
            );
            if urls.is_null() {
                return false;
            }
            let count = CFArrayGetCount(urls);
            CFRelease(urls);
            count > 0
        }
    }
}

#[cfg(windows)]
mod platform {
    #![allow(unsafe_code)]

    use windows::Win32::UI::Shell::{ASSOCF_NONE, ASSOCSTR_EXECUTABLE, AssocQueryStringW};
    use windows_core::{HSTRING, PWSTR};

    use super::{Failure, Outcome, Status};

    /// Settings' Default apps page for prev, as the installer registers it
    /// under RegisteredApplications. Windows versions without the
    /// per-app page show the Default apps list.
    const PAGE: &str = "ms-settings:defaultapps?registeredAppUser=prev";

    /// Windows names the default by prev.exe, which kept its name, so
    /// nothing moves.
    pub async fn move_old_defaults() {}

    pub async fn make_default() -> Result<Outcome, Failure> {
        crate::portal::open_uri(PAGE.to_owned())
            .await
            .map(|()| Outcome::OpenedSettings)
            .map_err(Failure::Other)
    }

    /// Counts the extensions whose default app is this prev.exe.
    pub fn status() -> Option<Status> {
        let exe = std::env::current_exe().ok()?;
        let name = exe.file_name()?.to_string_lossy().to_lowercase();
        let open = HSTRING::from("open");
        let set = crate::filetype::EXTENSIONS
            .iter()
            .filter(|extension| {
                let extension = HSTRING::from(format!(".{extension}"));
                let mut buffer = [0u16; 1024];
                let mut length = buffer.len() as u32;
                // SAFETY: the buffer and its length are valid for the call,
                // which writes at most `length` characters.
                let result = unsafe {
                    AssocQueryStringW(
                        ASSOCF_NONE,
                        ASSOCSTR_EXECUTABLE,
                        &extension,
                        &open,
                        Some(PWSTR(buffer.as_mut_ptr())),
                        &mut length,
                    )
                };
                if result.is_err() {
                    return false;
                }
                let end = buffer.iter().position(|&unit| unit == 0).unwrap_or(0);
                let path = String::from_utf16_lossy(&buffer[..end]);
                std::path::Path::new(&path)
                    .file_name()
                    .is_some_and(|file| file.to_string_lossy().to_lowercase() == name)
            })
            .count();
        Some(Status {
            set,
            total: crate::filetype::EXTENSIONS.len(),
        })
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::platform::set_defaults;

    const ID: &str = "prev.desktop";

    #[test]
    fn an_empty_list_gets_the_section() {
        assert_eq!(
            set_defaults("", &["application/pdf", "image/png"], ID),
            "[Default Applications]\napplication/pdf=prev.desktop\nimage/png=prev.desktop\n"
        );
    }

    #[test]
    fn other_entries_and_sections_stay() {
        let text = "[Added Associations]\nimage/png=gimp.desktop;\n\n[Default Applications]\napplication/pdf=evince.desktop\ntext/html=firefox.desktop\n\n[Removed Associations]\nimage/gif=eog.desktop;\n";
        let out = set_defaults(text, &["application/pdf", "image/png"], ID);
        assert_eq!(
            out,
            "[Added Associations]\nimage/png=gimp.desktop;\n\n[Default Applications]\ntext/html=firefox.desktop\napplication/pdf=prev.desktop\nimage/png=prev.desktop\n\n[Removed Associations]\nimage/gif=eog.desktop;\n"
        );
    }

    #[test]
    fn a_list_without_the_section_gets_it_at_the_end() {
        let out = set_defaults(
            "[Added Associations]\nimage/png=gimp.desktop;\n",
            &["image/png"],
            ID,
        );
        assert_eq!(
            out,
            "[Added Associations]\nimage/png=gimp.desktop;\n\n[Default Applications]\nimage/png=prev.desktop\n"
        );
    }

    #[test]
    fn the_first_listed_default_counts() {
        use super::platform::default_in;
        let text = "[Added Associations]\nimage/png=gimp.desktop;\n[Default Applications]\nimage/png=prev.desktop;gimp.desktop;\n";
        assert_eq!(
            default_in(text, "image/png").as_deref(),
            Some("prev.desktop")
        );
        assert_eq!(default_in(text, "image/gif"), None);
    }

    #[test]
    fn setting_twice_changes_nothing() {
        let once = set_defaults("", &["image/png"], ID);
        assert_eq!(set_defaults(&once, &["image/png"], ID), once);
    }

    #[test]
    fn the_old_entry_is_renamed_in_every_section() {
        use super::platform::rename_entry;
        let text = "[Added Associations]\napplication/pdf=old.desktop;\nimage/png=gimp.desktop;old.desktop;\n\n[Default Applications]\napplication/pdf=old.desktop\ntext/html=firefox.desktop\n# old.desktop=old.desktop\n";
        assert_eq!(
            rename_entry(text, "old.desktop", ID),
            "[Added Associations]\napplication/pdf=prev.desktop;\nimage/png=gimp.desktop;prev.desktop;\n\n[Default Applications]\napplication/pdf=prev.desktop\ntext/html=firefox.desktop\n# old.desktop=old.desktop\n"
        );
    }

    #[test]
    fn a_list_naming_both_names_the_new_one_once() {
        use super::platform::rename_entry;
        assert_eq!(
            rename_entry(
                "[Default Applications]\nimage/png=old.desktop;prev.desktop;\n",
                "old.desktop",
                ID
            ),
            "[Default Applications]\nimage/png=prev.desktop;\n"
        );
    }

    #[test]
    fn a_list_without_the_old_entry_stays_as_it_was() {
        use super::platform::rename_entry;
        let text = "[Default Applications]\nimage/png = gimp.desktop\nimage/gif=old.desktop.bak\n";
        assert_eq!(rename_entry(text, "old.desktop", ID), text);
    }
}
