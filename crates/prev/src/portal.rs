//! Opening links and printing, in prev's own words: through the XDG
//! desktop portal on Linux, and through the system itself elsewhere. The
//! desktop's settings, its accent color, light or dark and animations, are
//! read by scramble-ui.

#[cfg(target_os = "linux")]
pub use scramble_ui::desktop::color_scheme;
pub use scramble_ui::desktop::{accent_changes, accent_color, animations_enabled};

#[cfg(target_os = "linux")]
pub use linux::*;

/// Where a print without the dialog went.
#[derive(Debug, Clone, PartialEq)]
pub struct Sent {
    /// The printer, when the system says which.
    pub printer: Option<String>,
    /// Whether the print dialog showed instead, as in Flatpak, whose
    /// sandbox can only print through it.
    pub dialog: bool,
}

/// The printers a print without the dialog can go to.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Printers {
    pub names: Vec<String>,
    pub default: Option<String>,
    /// Whether prints can only go through the print dialog, as in
    /// Flatpak, which lists none.
    pub dialog_only: bool,
}

#[cfg(not(target_os = "linux"))]
pub use other::*;

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashMap;
    use std::os::fd::AsFd;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::time::Duration;

    use ashpd::desktop::ResponseError;
    use ashpd::desktop::print::{PrintOptions, PrintProxy};
    use ashpd::zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
    use ashpd::zbus::{self, Connection, Proxy};
    use iced::futures::StreamExt;
    use iced::futures::future::{self, Either};

    pub async fn open_uri(uri: String) -> Result<(), String> {
        let parsed = ashpd::Uri::parse(&uri).map_err(|error| {
            crate::fl!(
                "app-link-invalid",
                uri = uri.as_str(),
                error = error.to_string()
            )
        })?;
        ashpd::desktop::open_uri::OpenFileRequest::default()
            .send_uri(&parsed)
            .await
            .map(|_| ())
            .map_err(|error| {
                crate::fl!(
                    "app-link-open-failed",
                    uri = uri.as_str(),
                    error = error.to_string()
                )
            })
    }

    /// Prints the PDF at `path`, named `title` in the print queue, through
    /// the desktop's print dialog. When the print does not go through, the
    /// error says why, with what keeps printers from being used, which a
    /// cancelled dialog tells too, once a run.
    pub async fn print(path: PathBuf, title: String) -> Result<(), String> {
        // Looked into while the dialog is open, as the user means to print.
        let problem = crate::image::editor::spawn(printer_problem);
        let printed = match Connection::session().await {
            Ok(connection) => print_with(&connection, &path, &title).await,
            Err(error) => Err(dialog_error(error)),
        };
        let problem = || async {
            let waited = crate::image::editor::spawn(|| std::thread::sleep(PROBLEM_WAIT));
            match future::select(problem, waited).await {
                Either::Left((Ok(problem), _)) => problem.map(|problem| problem.describe()),
                _ => None,
            }
        };
        match printed {
            Ok(Printed::Sent) => Ok(()),
            Ok(Printed::Cancelled) => match problem().await {
                Some(problem) if !TOLD.swap(true, Ordering::Relaxed) => Err(problem),
                _ => Ok(()),
            },
            Err(error) => Err(match problem().await {
                Some(problem) => format!("{error} {problem}"),
                None => error,
            }),
        }
    }

    /// How long a print waits, once the dialog is done, to hear what keeps
    /// printers from being used.
    const PROBLEM_WAIT: Duration = Duration::from_secs(2);
    /// Whether a cancelled print has told what keeps printers from being
    /// used, which it does once a run.
    static TOLD: AtomicBool = AtomicBool::new(false);

    #[derive(Debug, PartialEq)]
    enum Printed {
        Sent,
        Cancelled,
    }

    async fn print_with(
        connection: &Connection,
        path: &Path,
        title: &str,
    ) -> Result<Printed, String> {
        let failed = |error: String| crate::fl!("print-failed", error = error);
        let Some(token) = prepare_print(connection, title)
            .await
            .map_err(dialog_error)?
        else {
            return Ok(Printed::Cancelled);
        };
        let file = std::fs::File::open(path).map_err(|error| failed(error.to_string()))?;
        let printed = PrintProxy::with_connection(connection.clone())
            .await
            .map_err(|error| failed(error.to_string()))?
            .print(
                None,
                title,
                &file.as_fd(),
                PrintOptions::default().set_token(token),
            )
            .await
            .map_err(|error| failed(error.to_string()))?;
        match printed.response() {
            Ok(()) => Ok(Printed::Sent),
            Err(ashpd::Error::Response(ResponseError::Cancelled)) => Ok(Printed::Cancelled),
            Err(error) => Err(failed(error.to_string())),
        }
    }

    /// Shows the print dialog, and gives the token to print with, or
    /// nothing if it was cancelled. ashpd's own call reads back every
    /// setting the dialog may answer with and fails on the first one
    /// missing, as most are, so only the token is read here.
    async fn prepare_print(connection: &Connection, title: &str) -> zbus::Result<Option<u32>> {
        const DESKTOP: &str = "org.freedesktop.portal.Desktop";
        // The dialog answers on a request named from this connection and
        // a token of prev's, listened to before asking so no answer is
        // missed.
        let sender = connection
            .unique_name()
            .map(|name| name.trim_start_matches(':').replace('.', "_"))
            .unwrap_or_default();
        static PRINTS: AtomicU32 = AtomicU32::new(0);
        let count = PRINTS.fetch_add(1, Ordering::Relaxed);
        let handle = format!("prev_print_{}_{count}", std::process::id());
        let request = Proxy::new(
            connection,
            DESKTOP,
            format!("/org/freedesktop/portal/desktop/request/{sender}/{handle}"),
            "org.freedesktop.portal.Request",
        )
        .await?;
        let mut answers = request.receive_signal("Response").await?;
        let print = Proxy::new(
            connection,
            DESKTOP,
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Print",
        )
        .await?;
        let none: HashMap<&str, Value<'_>> = HashMap::new();
        let options = HashMap::from([("handle_token", Value::from(handle.as_str()))]);
        let _: OwnedObjectPath = print
            .call("PreparePrint", &("", title, &none, &none, &options))
            .await?;
        let Some(answer) = answers.next().await else {
            return Ok(None);
        };
        let (response, results): (u32, HashMap<String, OwnedValue>) =
            answer.body().deserialize()?;
        token(response, &results)
    }

    /// The token in the print dialog's answer: none if it was cancelled
    /// or given up, which is any response but 0.
    fn token(response: u32, results: &HashMap<String, OwnedValue>) -> zbus::Result<Option<u32>> {
        if response != 0 {
            return Ok(None);
        }
        results
            .get("token")
            .and_then(|token| u32::try_from(token).ok())
            .map(Some)
            .ok_or_else(|| zbus::Error::Failure("the print dialog gave no token".into()))
    }

    /// The print dialog's failure in words: missing, where no portal has
    /// one.
    fn dialog_error(error: zbus::Error) -> String {
        const MISSING: &[&str] = &[
            "org.freedesktop.DBus.Error.ServiceUnknown",
            "org.freedesktop.DBus.Error.UnknownMethod",
            "org.freedesktop.DBus.Error.UnknownInterface",
            "org.freedesktop.DBus.Error.UnknownObject",
        ];
        let missing = match &error {
            zbus::Error::MethodError(name, ..) => MISSING.contains(&name.as_str()),
            zbus::Error::FDO(error) => matches!(
                **error,
                zbus::fdo::Error::ServiceUnknown(_)
                    | zbus::fdo::Error::UnknownMethod(_)
                    | zbus::fdo::Error::UnknownInterface(_)
                    | zbus::fdo::Error::UnknownObject(_)
            ),
            _ => false,
        };
        if missing {
            crate::fl!("print-no-dialog")
        } else {
            crate::fl!("print-failed", error = error.to_string())
        }
    }

    /// What keeps printers from being used.
    #[derive(Debug, PartialEq)]
    enum Problem {
        NoCups,
        CupsStopped,
        NoPrinter,
        /// A printer CUPS finds on the network, but has no queue for, which
        /// some print dialogs wait on for good.
        NotSetUp(String),
    }

    impl Problem {
        fn describe(&self) -> String {
            match self {
                Problem::NoCups => crate::fl!("print-no-cups"),
                Problem::CupsStopped => crate::fl!("print-cups-stopped"),
                Problem::NoPrinter => crate::fl!("print-no-printer"),
                Problem::NotSetUp(printer) => {
                    crate::fl!("print-not-set-up", printer = printer.as_str())
                }
            }
        }
    }

    /// Prints the PDF at `path` without the dialog, named `title` in the
    /// print queue, to `printer` or CUPS's default, with lp. In Flatpak,
    /// whose sandbox has no CUPS, the print dialog shows instead.
    pub async fn send(
        path: PathBuf,
        title: String,
        printer: Option<String>,
    ) -> Result<super::Sent, String> {
        if in_flatpak() {
            let connection = Connection::session().await.map_err(dialog_error)?;
            return match print_with(&connection, &path, &title).await? {
                Printed::Sent => Ok(super::Sent {
                    printer: None,
                    dialog: true,
                }),
                Printed::Cancelled => Err("The user cancelled the print dialog.".to_owned()),
            };
        }
        crate::image::editor::spawn(move || lp(&path, &title, printer.as_deref()))
            .await
            .unwrap_or_else(|_| Err(crate::fl!("print-stopped")))
    }

    /// The printers CUPS knows of, and its default one.
    pub async fn printers() -> Result<super::Printers, String> {
        if in_flatpak() {
            return Ok(super::Printers {
                dialog_only: true,
                ..super::Printers::default()
            });
        }
        crate::image::editor::spawn(|| {
            let Some(known) = lpstat("-e") else {
                return Err(Problem::NoCups.describe());
            };
            Ok(super::Printers {
                names: known
                    .lines()
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned)
                    .collect(),
                default: lpstat("-d").as_deref().and_then(default_printer),
                dialog_only: false,
            })
        })
        .await
        .unwrap_or_else(|_| Err(crate::fl!("print-stopped")))
    }

    fn in_flatpak() -> bool {
        std::env::var_os("FLATPAK_ID").is_some()
    }

    /// Hands the PDF at `path` to CUPS. Its answers, for agents, are in
    /// English, as lp gives them with LC_ALL=C.
    fn lp(path: &Path, title: &str, printer: Option<&str>) -> Result<super::Sent, String> {
        let mut command = Command::new("lp");
        if let Some(printer) = printer {
            command.args(["-d", printer]);
        }
        let output = command
            .args(["-t", title, "--"])
            .arg(path)
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .output()
            .map_err(|_| Problem::NoCups.describe())?;
        if !output.status.success() {
            let said = String::from_utf8_lossy(&output.stderr);
            let said = said.trim().trim_start_matches("lp: ");
            return Err(if said.contains("No default destination") {
                "There is no default printer; name one from list_printers.".to_owned()
            } else {
                said.to_owned()
            });
        }
        Ok(super::Sent {
            printer: queued_on(&String::from_utf8_lossy(&output.stdout))
                .or_else(|| printer.map(str::to_owned)),
            dialog: false,
        })
    }

    /// The printer in lp's answer, such as "request id is
    /// EPSON_ET-4800-12 (1 file(s))".
    fn queued_on(answer: &str) -> Option<String> {
        let id = answer
            .trim()
            .strip_prefix("request id is ")?
            .split_whitespace()
            .next()?;
        id.rsplit_once('-').map(|(printer, _)| printer.to_owned())
    }

    /// The default printer in lpstat -d's answer, such as "system default
    /// destination: EPSON_ET-4800".
    fn default_printer(answer: &str) -> Option<String> {
        let name = answer
            .trim()
            .strip_prefix("system default destination:")?
            .trim();
        (!name.is_empty()).then(|| name.to_owned())
    }

    /// What lpstat answers to `option`, read in English; nothing where it
    /// is not installed.
    fn lpstat(option: &str) -> Option<String> {
        let output = Command::new("lpstat")
            .arg(option)
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// What keeps printers from being used, as CUPS's lpstat tells it;
    /// nothing in Flatpak, whose sandbox does not see CUPS, though its
    /// print dialog does.
    fn printer_problem() -> Option<Problem> {
        if in_flatpak() {
            return None;
        }
        let Some(scheduler) = lpstat("-r") else {
            return Some(Problem::NoCups);
        };
        problem(
            &scheduler,
            &lpstat("-a").unwrap_or_default(),
            &lpstat("-e").unwrap_or_default(),
        )
    }

    /// The problem in lpstat's answers: `scheduler` from `-r`, the queues
    /// set up from `-a`, and every printer CUPS knows of from `-e`.
    fn problem(scheduler: &str, queues: &str, printers: &str) -> Option<Problem> {
        if !scheduler.contains("scheduler is running") {
            return Some(Problem::CupsStopped);
        }
        if queues.lines().any(|line| !line.trim().is_empty()) {
            return None;
        }
        match printers
            .lines()
            .map(str::trim)
            .find(|name| !name.is_empty())
        {
            Some(name) => Some(Problem::NotSetUp(name.replace('_', " "))),
            None => Some(Problem::NoPrinter),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use iced::futures::executor::block_on;
        use std::io::{BufRead, BufReader, Write};
        use std::process::Child;
        use std::sync::{Arc, Mutex};

        #[test]
        fn lpstat_tells_what_keeps_printers_from_being_used() {
            let running = "scheduler is running\n";
            let queue = "EPSON_ET-4800 accepting requests since Sun 25 Oct 2026\n";
            assert_eq!(problem(running, queue, "EPSON_ET-4800\n"), None);
            assert_eq!(
                problem(running, "", "EPSON_ET_4800_Series\n"),
                Some(Problem::NotSetUp("EPSON ET 4800 Series".to_owned()))
            );
            assert_eq!(problem(running, "", ""), Some(Problem::NoPrinter));
            assert_eq!(
                problem("scheduler is not running\n", "", ""),
                Some(Problem::CupsStopped)
            );
        }

        #[test]
        fn lp_and_lpstat_name_the_printer() {
            assert_eq!(
                queued_on("request id is EPSON_ET-4800-12 (1 file(s))\n").as_deref(),
                Some("EPSON_ET-4800")
            );
            assert_eq!(queued_on(""), None);
            assert_eq!(
                default_printer("system default destination: EPSON_ET-4800\n").as_deref(),
                Some("EPSON_ET-4800")
            );
            assert_eq!(default_printer("no system default destination\n"), None);
        }

        #[test]
        fn the_dialogs_answer_needs_only_its_token() {
            let answer = HashMap::from([("token".to_owned(), OwnedValue::from(7u32))]);
            assert_eq!(token(0, &answer).unwrap(), Some(7));
            assert_eq!(token(1, &HashMap::new()).unwrap(), None);
            assert!(token(0, &HashMap::new()).is_err());
        }

        #[test]
        fn prints_when_the_dialog_answers_with_only_a_token() {
            let Some(bus) = Bus::start() else {
                return;
            };
            block_on(async {
                let (_portal, printed) = bus.portal(Some(false)).await;
                let file = tempfile::NamedTempFile::new().unwrap();
                let sent = print_with(&bus.connect().await, file.path(), "test").await;
                assert_eq!(sent, Ok(Printed::Sent));
                assert_eq!(*printed.lock().unwrap(), [7]);
            });
        }

        #[test]
        fn a_cancelled_dialog_prints_nothing() {
            let Some(bus) = Bus::start() else {
                return;
            };
            block_on(async {
                let (_portal, printed) = bus.portal(Some(true)).await;
                let file = tempfile::NamedTempFile::new().unwrap();
                let sent = print_with(&bus.connect().await, file.path(), "test").await;
                assert_eq!(sent, Ok(Printed::Cancelled));
                assert!(printed.lock().unwrap().is_empty());
            });
        }

        #[test]
        fn a_portal_without_a_print_dialog_says_so() {
            let Some(bus) = Bus::start() else {
                return;
            };
            block_on(async {
                let _portal = bus.portal(None).await;
                let file = tempfile::NamedTempFile::new().unwrap();
                let sent = print_with(&bus.connect().await, file.path(), "test").await;
                assert_eq!(sent, Err(crate::fl!("print-no-dialog")));
            });
        }

        /// A session bus of the test's own, which starts no services.
        struct Bus {
            daemon: Child,
            address: String,
            _config: tempfile::NamedTempFile,
        }

        impl Bus {
            /// The bus, or nothing where there is no dbus-daemon, except in
            /// CI, which must run these tests.
            fn start() -> Option<Self> {
                let started = Self::try_start();
                if started.is_none() {
                    assert!(
                        std::env::var_os("CI").is_none(),
                        "dbus-daemon is needed for the print portal tests"
                    );
                    eprintln!("no dbus-daemon, so the print portal is not tested");
                }
                started
            }

            fn try_start() -> Option<Self> {
                let mut config = tempfile::NamedTempFile::new().ok()?;
                write!(
                    config,
                    "<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen>\
                     <auth>EXTERNAL</auth><policy context=\"default\">\
                     <allow send_destination=\"*\" eavesdrop=\"true\"/>\
                     <allow eavesdrop=\"true\"/><allow own=\"*\"/></policy></busconfig>"
                )
                .ok()?;
                let mut daemon = Command::new("dbus-daemon")
                    .arg(format!("--config-file={}", config.path().display()))
                    .args(["--nofork", "--print-address"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                    .ok()?;
                let mut address = String::new();
                BufReader::new(daemon.stdout.take()?)
                    .read_line(&mut address)
                    .ok()?;
                Some(Self {
                    daemon,
                    address: address.trim().to_owned(),
                    _config: config,
                })
            }

            async fn connect(&self) -> Connection {
                zbus::connection::Builder::address(self.address.as_str())
                    .unwrap()
                    .build()
                    .await
                    .unwrap()
            }

            /// The desktop portal, with a print dialog that cancels or not,
            /// or only another portal's, and the tokens printed with.
            async fn portal(&self, cancels: Option<bool>) -> (Connection, Arc<Mutex<Vec<u32>>>) {
                let printed = Arc::new(Mutex::new(Vec::new()));
                let mut builder = zbus::connection::Builder::address(self.address.as_str())
                    .unwrap()
                    .name("org.freedesktop.portal.Desktop")
                    .unwrap();
                builder = match cancels {
                    Some(cancels) => {
                        let dialog = Dialog {
                            cancels,
                            printed: Arc::clone(&printed),
                        };
                        builder.serve_at("/org/freedesktop/portal/desktop", dialog)
                    }
                    None => builder.serve_at("/org/freedesktop/portal/desktop", OpenUri),
                }
                .unwrap();
                (builder.build().await.unwrap(), printed)
            }
        }

        impl Drop for Bus {
            fn drop(&mut self) {
                let _ = self.daemon.kill();
                let _ = self.daemon.wait();
            }
        }

        /// A portal other than the print dialog, as a desktop without one
        /// has.
        struct OpenUri;

        #[zbus::interface(name = "org.freedesktop.portal.OpenURI")]
        impl OpenUri {
            #[zbus(property)]
            fn version(&self) -> u32 {
                4
            }
        }

        /// The print portal, answering as GTK's does: with a token and no
        /// settings, or cancelled.
        struct Dialog {
            cancels: bool,
            printed: Arc<Mutex<Vec<u32>>>,
        }

        /// Where a request from `header`'s sender, with `options`'
        /// handle token, is answered.
        fn request_path(
            header: &zbus::message::Header<'_>,
            options: &HashMap<String, OwnedValue>,
        ) -> OwnedObjectPath {
            let sender = header
                .sender()
                .map(|name| name.trim_start_matches(':').replace('.', "_"))
                .unwrap_or_default();
            let handle = options
                .get("handle_token")
                .and_then(|token| String::try_from(token.try_clone().ok()?).ok())
                .unwrap_or_default();
            OwnedObjectPath::try_from(format!(
                "/org/freedesktop/portal/desktop/request/{sender}/{handle}"
            ))
            .unwrap()
        }

        #[zbus::interface(name = "org.freedesktop.portal.Print")]
        impl Dialog {
            #[allow(clippy::too_many_arguments)]
            async fn prepare_print(
                &self,
                #[zbus(header)] header: zbus::message::Header<'_>,
                #[zbus(connection)] connection: &Connection,
                _parent: &str,
                _title: &str,
                _settings: HashMap<String, OwnedValue>,
                _page_setup: HashMap<String, OwnedValue>,
                options: HashMap<String, OwnedValue>,
            ) -> zbus::fdo::Result<OwnedObjectPath> {
                let path = request_path(&header, &options);
                let mut results: HashMap<&str, Value<'_>> = HashMap::new();
                if !self.cancels {
                    let empty: HashMap<&str, Value<'_>> = HashMap::new();
                    results.insert("settings", Value::from(empty.clone()));
                    results.insert("page-setup", Value::from(empty));
                    results.insert("token", Value::from(7u32));
                }
                let response = u32::from(self.cancels);
                connection
                    .emit_signal(
                        None::<()>,
                        &path,
                        "org.freedesktop.portal.Request",
                        "Response",
                        &(response, results),
                    )
                    .await?;
                Ok(path)
            }

            async fn print(
                &self,
                #[zbus(header)] header: zbus::message::Header<'_>,
                #[zbus(connection)] connection: &Connection,
                _parent: &str,
                _title: &str,
                _file: zbus::zvariant::OwnedFd,
                options: HashMap<String, OwnedValue>,
            ) -> zbus::fdo::Result<OwnedObjectPath> {
                if let Some(token) = options
                    .get("token")
                    .and_then(|token| u32::try_from(token).ok())
                {
                    self.printed.lock().unwrap().push(token);
                }
                let path = request_path(&header, &options);
                let results: HashMap<&str, Value<'_>> = HashMap::new();
                connection
                    .emit_signal(
                        None::<()>,
                        &path,
                        "org.freedesktop.portal.Request",
                        "Response",
                        &(0u32, results),
                    )
                    .await?;
                Ok(path)
            }

            #[zbus(property)]
            fn version(&self) -> u32 {
                4
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod other {
    use std::path::PathBuf;

    /// Opens `uri` in the default app: through the URL handler every
    /// Windows version has, or `open` on macOS.
    pub async fn open_uri(uri: String) -> Result<(), String> {
        #[cfg(windows)]
        let mut command = std::process::Command::new("rundll32");
        #[cfg(windows)]
        command.args(["url.dll,FileProtocolHandler", &uri]);
        #[cfg(not(windows))]
        let mut command = std::process::Command::new("open");
        #[cfg(not(windows))]
        command.arg(&uri);
        command.spawn().map(|_| ()).map_err(|error| {
            crate::fl!(
                "app-link-open-failed",
                uri = uri.as_str(),
                error = error.to_string()
            )
        })
    }

    /// Prints through the Windows print dialog, on a thread of its own:
    /// the dialog and the spooling block until they are done.
    #[cfg(windows)]
    pub async fn print(path: PathBuf, title: String) -> Result<(), String> {
        let (sender, receiver) = iced::futures::channel::oneshot::channel();
        std::thread::Builder::new()
            .name("prev-print".into())
            .spawn(move || {
                let _ = sender.send(crate::print_windows::print(&path, &title));
            })
            .map_err(|error| crate::fl!("print-failed", error = error.to_string()))?;
        receiver
            .await
            .unwrap_or_else(|_| Err(crate::fl!("print-stopped")))
    }

    /// Prints through AppKit's print panel, which must run on the main
    /// thread; this waits for it there.
    #[cfg(target_os = "macos")]
    pub async fn print(path: PathBuf, title: String) -> Result<(), String> {
        let (sender, receiver) = iced::futures::channel::oneshot::channel();
        crate::print_macos::on_main(move |mtm| {
            let _ = sender.send(crate::print_macos::print_on_main(mtm, &path, &title));
        });
        receiver
            .await
            .unwrap_or_else(|_| Err(crate::fl!("print-unavailable")))
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    pub async fn print(_path: PathBuf, _title: String) -> Result<(), String> {
        Err(crate::fl!("print-unavailable"))
    }

    /// Prints the PDF at `path` without the dialog, named `title` in the
    /// print queue, to `printer` or the system's default.
    #[cfg(windows)]
    pub async fn send(
        path: PathBuf,
        title: String,
        printer: Option<String>,
    ) -> Result<super::Sent, String> {
        let (sender, receiver) = iced::futures::channel::oneshot::channel();
        std::thread::Builder::new()
            .name("prev-print".into())
            .spawn(move || {
                let _ = sender.send(crate::print_windows::print_to(
                    &path,
                    &title,
                    printer.as_deref(),
                ));
            })
            .map_err(|error| crate::fl!("print-failed", error = error.to_string()))?;
        let printer = receiver
            .await
            .unwrap_or_else(|_| Err(crate::fl!("print-stopped")))?;
        Ok(super::Sent {
            printer: Some(printer),
            dialog: false,
        })
    }

    #[cfg(target_os = "macos")]
    pub async fn send(
        path: PathBuf,
        title: String,
        printer: Option<String>,
    ) -> Result<super::Sent, String> {
        let (sender, receiver) = iced::futures::channel::oneshot::channel();
        crate::print_macos::on_main(move |mtm| {
            let _ = sender.send(crate::print_macos::print_to_on_main(
                mtm,
                &path,
                &title,
                printer.as_deref(),
            ));
        });
        let printer = receiver
            .await
            .unwrap_or_else(|_| Err(crate::fl!("print-unavailable")))?;
        Ok(super::Sent {
            printer: Some(printer),
            dialog: false,
        })
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    pub async fn send(
        _path: PathBuf,
        _title: String,
        _printer: Option<String>,
    ) -> Result<super::Sent, String> {
        Err(crate::fl!("print-unavailable"))
    }

    /// The system's printers and its default one.
    #[cfg(windows)]
    pub async fn printers() -> Result<super::Printers, String> {
        Ok(crate::print_windows::printers())
    }

    #[cfg(target_os = "macos")]
    pub async fn printers() -> Result<super::Printers, String> {
        let (sender, receiver) = iced::futures::channel::oneshot::channel();
        crate::print_macos::on_main(move |mtm| {
            let _ = sender.send(crate::print_macos::printers_on_main(mtm));
        });
        receiver.await.map_err(|_| crate::fl!("print-unavailable"))
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    pub async fn printers() -> Result<super::Printers, String> {
        Err(crate::fl!("print-unavailable"))
    }
}
