//! Pure, explicit GUI routing. Arguments exclude the executable name.
//! No commands, process operations, shell expansion or fuzzy flag matching.
use anyhow::{Result, bail, ensure};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchPage {
    Overview,
    Processes,
    Details,
    Performance,
    Ports,
    Services,
    Startup,
    Users,
    History,
    Devices,
    Settings,
}
impl LaunchPage {
    fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "overview" => Self::Overview,
            "processes" => Self::Processes,
            "details" => Self::Details,
            "performance" => Self::Performance,
            "ports" => Self::Ports,
            "services" => Self::Services,
            "startup" => Self::Startup,
            "users" => Self::Users,
            "history" => Self::History,
            "devices" => Self::Devices,
            "settings" => Self::Settings,
            _ => bail!(
                "Unknown page '{value}'. Use overview, processes, details, performance, ports, services, startup, users, history, devices, or settings."
            ),
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchView {
    Simple,
    Developer,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaunchRequest {
    /// None leaves the normal default page selection to the application.
    pub page: Option<LaunchPage>,
    /// The canonical, literal query for a PID or port route, otherwise None.
    pub query: Option<String>,
    /// None preserves the application's saved column layout.
    pub view: Option<LaunchView>,
    /// Opens application help; does not print or execute anything here.
    pub show_help: bool,
}
impl LaunchRequest {
    /// Parse arguments after argv[0], e.g. env::args().skip(1).
    /// Exactly one of --pid, --port, --page is allowed; repeated switches,
    /// unknown arguments, missing values, and conflicting modes are errors.
    pub fn parse<I, S>(arguments: I) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut request = Self::default();
        let mut arguments = arguments.into_iter();
        while let Some(argument) = arguments.next() {
            match argument.as_ref() {
                "--pid" => {
                    request.ensure_no_route()?;
                    let value = arguments
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--pid requires a process ID"))?;
                    let pid = decimal(value.as_ref(), "--pid", u32::MAX)?;
                    request.page = Some(LaunchPage::Processes);
                    request.query = Some(format!("pid:{pid}"));
                }
                "--port" => {
                    request.ensure_no_route()?;
                    let value = arguments.next().ok_or_else(|| {
                        anyhow::anyhow!("--port requires a local or remote port number")
                    })?;
                    let port = decimal(value.as_ref(), "--port", u32::from(u16::MAX))?;
                    request.page = Some(LaunchPage::Ports);
                    request.query = Some(format!("port:{port}"));
                }
                "--page" => {
                    request.ensure_no_route()?;
                    let value = arguments
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--page requires a page name"))?;
                    ensure!(
                        !value.as_ref().starts_with("--"),
                        "--page requires a page name before the next flag"
                    );
                    request.page = Some(LaunchPage::parse(value.as_ref())?);
                }
                "--simple" => {
                    ensure!(
                        request.view.is_none(),
                        "Choose only one view flag: --simple or --developer; repeated view flags are not allowed"
                    );
                    request.view = Some(LaunchView::Simple);
                }
                "--developer" => {
                    ensure!(
                        request.view.is_none(),
                        "Choose only one view flag: --simple or --developer; repeated view flags are not allowed"
                    );
                    request.view = Some(LaunchView::Developer);
                }
                "--help" => {
                    ensure!(!request.show_help, "--help cannot be repeated");
                    request.show_help = true;
                }
                unknown => bail!(
                    "Unknown launch argument '{unknown}'. Use --help to see supported options."
                ),
            }
        }
        Ok(request)
    }
    fn ensure_no_route(&self) -> Result<()> {
        ensure!(
            self.page.is_none(),
            "Choose only one route: --pid, --port, or --page; repeated route flags are not allowed"
        );
        Ok(())
    }
}
fn decimal(value: &str, flag: &str, maximum: u32) -> Result<u32> {
    ensure!(
        !value.starts_with("--"),
        "{flag} requires a number before the next flag"
    );
    ensure!(
        !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()),
        "{flag} requires an unsigned ASCII decimal number without signs or spaces"
    );
    let number = value.parse::<u32>().map_err(|_| {
        anyhow::anyhow!("{flag} number is outside the supported range 1..={maximum}")
    })?;
    ensure!(
        (1..=maximum).contains(&number),
        "{flag} number is outside the supported range 1..={maximum}"
    );
    Ok(number)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_arguments_preserve_normal_application_defaults() {
        assert_eq!(
            LaunchRequest::parse(std::iter::empty::<&str>()).unwrap(),
            LaunchRequest::default()
        );
    }
    #[test]
    fn pid_routes_to_processes_with_canonical_query_and_full_u32_precision() {
        for (value, expected) in [
            ("1", "pid:1"),
            ("0008123", "pid:8123"),
            ("4294967295", "pid:4294967295"),
        ] {
            let request = LaunchRequest::parse(["--pid", value]).unwrap();
            assert_eq!(request.page, Some(LaunchPage::Processes));
            assert_eq!(request.query.as_deref(), Some(expected));
            assert_eq!(request.view, None);
        }
    }
    #[test]
    fn port_routes_accept_both_valid_boundaries_and_reject_zero_overflow() {
        for (value, expected) in [("1", "port:1"), ("65535", "port:65535")] {
            let request = LaunchRequest::parse(["--port", value]).unwrap();
            assert_eq!(request.page, Some(LaunchPage::Ports));
            assert_eq!(request.query.as_deref(), Some(expected));
        }
        for value in ["0", "65536", "4294967295", "4294967296"] {
            assert!(LaunchRequest::parse(["--port", value]).is_err());
        }
    }
    #[test]
    fn every_page_name_routes_without_inventing_a_query() {
        for (name, page) in [
            ("overview", LaunchPage::Overview),
            ("processes", LaunchPage::Processes),
            ("details", LaunchPage::Details),
            ("performance", LaunchPage::Performance),
            ("ports", LaunchPage::Ports),
            ("services", LaunchPage::Services),
            ("startup", LaunchPage::Startup),
            ("users", LaunchPage::Users),
            ("history", LaunchPage::History),
            ("devices", LaunchPage::Devices),
            ("settings", LaunchPage::Settings),
        ] {
            let request = LaunchRequest::parse(["--page", name]).unwrap();
            assert_eq!(request.page, Some(page));
            assert_eq!(request.query, None);
        }
    }
    #[test]
    fn help_is_independent_of_route_and_mode_order() {
        let alone = LaunchRequest::parse(["--help"]).unwrap();
        assert!(alone.show_help && alone.page.is_none());
        let first = LaunchRequest::parse(["--help", "--developer", "--page", "ports"]).unwrap();
        let last = LaunchRequest::parse(["--page", "ports", "--developer", "--help"]).unwrap();
        assert_eq!(first, last);
        assert!(first.show_help);
        assert_eq!(first.view, Some(LaunchView::Developer));
        assert_eq!(
            LaunchRequest::parse(["--simple", "--pid", "23"])
                .unwrap()
                .view,
            Some(LaunchView::Simple)
        );
    }
    #[test]
    fn repeated_flags_and_every_route_pair_are_rejected() {
        for arguments in [
            vec!["--pid", "1", "--pid", "2"],
            vec!["--pid", "1", "--port", "80"],
            vec!["--port", "80", "--pid", "1"],
            vec!["--pid", "1", "--page", "processes"],
            vec!["--page", "ports", "--port", "80"],
            vec!["--page", "ports", "--page", "ports"],
            vec!["--port", "80", "--port", "80"],
            vec!["--simple", "--simple"],
            vec!["--developer", "--developer"],
            vec!["--simple", "--developer"],
            vec!["--developer", "--simple"],
            vec!["--help", "--help"],
        ] {
            assert!(LaunchRequest::parse(arguments).is_err());
        }
    }
    #[test]
    fn missing_values_are_not_reinterpreted_as_following_flags() {
        for arguments in [
            vec!["--pid"],
            vec!["--port"],
            vec!["--page"],
            vec!["--pid", "--help"],
            vec!["--port", "--simple"],
            vec!["--page", "--developer"],
        ] {
            let error = LaunchRequest::parse(arguments).unwrap_err().to_string();
            assert!(error.contains("requires"), "{error}");
        }
    }
    #[test]
    fn signed_non_ascii_empty_whitespace_and_overflow_pid_values_are_errors() {
        for value in [
            "",
            "0",
            "+1",
            "-1",
            " 12",
            "12 ",
            "1.5",
            "0x10",
            "１２",
            "١٢",
            "4294967296",
            "18446744073709551615",
            "12\n",
            "12\0",
        ] {
            assert!(LaunchRequest::parse(["--pid", value]).is_err(), "{value}");
        }
    }
    #[test]
    fn unknown_unicode_case_and_inline_forms_are_never_guessed() {
        for arguments in [
            vec!["--PAGE", "ports"],
            vec!["--pid=12"],
            vec!["--port=80"],
            vec!["--page", "Ports"],
            vec!["--page", "端口"],
            vec!["--未知"],
            vec!["ports"],
            vec!["--"],
            vec!["--help", "unexpected"],
        ] {
            assert!(LaunchRequest::parse(arguments).is_err());
        }
    }
}
