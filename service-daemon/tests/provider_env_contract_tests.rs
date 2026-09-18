use service_daemon::{ManagedProvided, Provided, provider};
use std::process::Command;

const KEY: &str = "SD_TEST_ENV_VALUE_CONTRACT";
const CHILD: &str = "SD_TEST_ENV_CONTRACT_CHILD";

#[provider(true, env = "SD_TEST_ENV_VALUE_CONTRACT")]
#[derive(Clone)]
struct DefaultTrue(bool);
#[provider(false, env = "SD_TEST_ENV_VALUE_CONTRACT")]
#[derive(Clone)]
struct DefaultFalse(bool);
#[provider(env = "SD_TEST_ENV_VALUE_CONTRACT")]
#[derive(Clone)]
struct RequiredBool(bool);
#[provider(7, env = "SD_TEST_ENV_VALUE_CONTRACT")]
#[derive(Clone)]
struct DefaultNumber(u16);
#[provider(env = "SD_TEST_ENV_VALUE_CONTRACT")]
#[derive(Clone)]
struct RequiredNumber(u16);
#[provider("fallback", env = "SD_TEST_ENV_VALUE_CONTRACT")]
#[derive(Clone)]
struct DefaultString(String);
#[provider(env = "SD_TEST_ENV_VALUE_CONTRACT")]
#[derive(Clone)]
struct RequiredString(String);

type Flag = bool;
type Text = String;
#[provider(env = "SD_TEST_ENV_VALUE_CONTRACT")]
#[derive(Clone)]
struct AliasedBool(Flag);
#[provider(String::from("fallback"), env = "SD_TEST_ENV_VALUE_CONTRACT")]
#[derive(Clone)]
struct AliasedString(Text);
#[provider(env = "SD_TEST_ENV_VALUE_CONTRACT")]
#[derive(Clone)]
struct QualifiedBool(std::primitive::bool);

#[derive(Clone, Debug, PartialEq)]
struct CustomNumber(u16);
impl std::fmt::Display for CustomNumber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::str::FromStr for CustomNumber {
    type Err = std::num::ParseIntError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value.parse().map(Self)
    }
}
#[provider(CustomNumber(7), env = "SD_TEST_ENV_VALUE_CONTRACT")]
#[derive(Clone)]
struct DefaultCustom(CustomNumber);
#[provider(env = "SD_TEST_ENV_VALUE_CONTRACT")]
#[derive(Clone)]
struct RequiredCustom(CustomNumber);
#[provider(1.5, env = "SD_TEST_ENV_VALUE_CONTRACT")]
#[derive(Clone)]
struct DefaultFloat(f64);

#[test]
fn env_contract_matrix() {
    for value in [
        None,
        Some(""),
        Some(" \t\n"),
        Some(" FALSE "),
        Some("Off"),
        Some(" 0 "),
        Some("true"),
        Some("ON"),
        Some("1"),
        Some("no"),
        Some("NO"),
        Some("nO"),
        Some(" no "),
        Some("00"),
        Some("0.0"),
        Some("-1"),
        Some(" 42 "),
        Some("\u{2003}42\u{2003}"),
        Some("bad"),
        Some(" 2.5 "),
    ] {
        for mode in ["snapshot", "managed", "rwlock", "mutex"] {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args(["--exact", "env_contract_child", "--nocapture"])
                .env(CHILD, mode)
                .env_remove(KEY);
            if let Some(value) = value {
                command.env(KEY, value);
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "value={value:?}, mode={mode}\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn unreadable_env_preserves_fallback_and_required_failure() {
    use std::os::unix::ffi::OsStrExt;
    for mode in ["snapshot", "managed", "rwlock", "mutex"] {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "env_contract_child", "--nocapture"])
            .env(CHILD, mode)
            .env(KEY, std::ffi::OsStr::from_bytes(b"\xff"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[tokio::test]
async fn env_contract_child() {
    let Ok(mode) = std::env::var(CHILD) else {
        return;
    };
    let raw = std::env::var(KEY).ok();
    let trimmed = raw.as_deref().map(str::trim).filter(|v| !v.is_empty());
    let boolean = trimmed.map(|v| {
        !(v.eq_ignore_ascii_case("false")
            || v.eq_ignore_ascii_case("off")
            || v.eq_ignore_ascii_case("no")
            || v == "0")
    });
    assert_eq!(DefaultTrue::default().0, boolean.unwrap_or(true));
    assert_eq!(DefaultFalse::default().0, boolean.unwrap_or(false));
    let number = trimmed.and_then(|v| v.parse::<u16>().ok());
    assert_eq!(DefaultNumber::default().0, number.unwrap_or(7));
    let string = raw.as_deref().filter(|v| !v.is_empty());
    assert_eq!(DefaultString::default().0, string.unwrap_or("fallback"));
    assert_eq!(AliasedString::default().0, string.unwrap_or("fallback"));
    assert_eq!(
        DefaultCustom::default().0,
        CustomNumber(number.unwrap_or(7))
    );
    assert_eq!(
        DefaultFloat::default().0,
        trimmed.and_then(|v| v.parse::<f64>().ok()).unwrap_or(1.5)
    );

    // Separate subprocesses exercise both constructors without cached snapshots.
    macro_rules! resolve {
        ($ty:ty) => {
            if mode == "managed" {
                <$ty>::resolve_managed().await.map_err(|e| format!("{e:?}"))
            } else if mode == "rwlock" {
                match <$ty as ManagedProvided>::resolve_rwlock().await {
                    Ok(value) => Ok(std::sync::Arc::new(value.read().await.clone())),
                    Err(error) => Err(error.to_string()),
                }
            } else if mode == "mutex" {
                match <$ty as ManagedProvided>::resolve_mutex().await {
                    Ok(value) => Ok(std::sync::Arc::new(value.lock().await.clone())),
                    Err(error) => Err(error.to_string()),
                }
            } else {
                <$ty as Provided>::resolve()
                    .await
                    .map_err(|e| e.to_string())
            }
        };
    }
    assert_eq!(resolve!(DefaultTrue).unwrap().0, boolean.unwrap_or(true));
    assert_eq!(resolve!(DefaultFalse).unwrap().0, boolean.unwrap_or(false));
    assert_eq!(resolve!(DefaultNumber).unwrap().0, number.unwrap_or(7));
    assert_eq!(
        resolve!(DefaultString).unwrap().0,
        string.unwrap_or("fallback")
    );
    assert_eq!(resolve!(AliasedBool).ok().map(|v| v.0), boolean);
    assert_eq!(resolve!(QualifiedBool).ok().map(|v| v.0), boolean);
    assert_eq!(
        resolve!(RequiredCustom).ok().map(|v| v.0.clone()),
        number.map(CustomNumber)
    );
    let result = resolve!(RequiredBool);
    match boolean {
        Some(value) => assert_eq!(result.unwrap().0, value),
        None => assert!(result.err().unwrap().contains("not set")),
    }
    let result = resolve!(RequiredNumber);
    match number {
        Some(value) => assert_eq!(result.unwrap().0, value),
        None => assert!(result.err().unwrap().contains(if trimmed.is_none() {
            "not set"
        } else {
            "cannot be parsed"
        })),
    }
    let result = resolve!(RequiredString);
    match string {
        Some(value) => assert_eq!(result.unwrap().0, value),
        None => assert!(result.err().unwrap().contains("not set")),
    }
}
