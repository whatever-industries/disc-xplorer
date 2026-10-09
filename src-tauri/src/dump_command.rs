//! Editable redumper arguments; commands are never passed to a shell.
use super::{dump_args, DumpRequest};
use std::path::Path;

pub fn split(input: &str) -> Result<Vec<String>, String> {
    if input.len() > 16384 || input.contains('\0') {
        return Err("Command is too long or contains invalid characters.".into());
    }
    let mut args = Vec::new();
    let mut token = String::new();
    let mut quote = None;
    let mut started = false;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && quote != Some('\'') {
            let escaped = chars.peek().is_some_and(|next| {
                *next == '\\'
                    || *next == '"'
                    || (quote.is_none() && (*next == '\'' || next.is_whitespace()))
            });
            if escaped {
                token.push(chars.next().unwrap());
            } else {
                token.push(c); // Keep Windows path separators.
            }
            started = true;
        } else if let Some(q) = quote {
            if c == q {
                quote = None;
            } else {
                token.push(c);
            }
        } else if c == '\'' || c == '"' {
            quote = Some(c);
            started = true;
        } else if c.is_whitespace() {
            if started {
                args.push(std::mem::take(&mut token));
                started = false;
            }
        } else if ";&|<>`$".contains(c) {
            return Err("Enter redumper arguments only; shell syntax is not supported.".into());
        } else {
            token.push(c);
            started = true;
        }
    }
    if quote.is_some() {
        return Err("Close the quotation mark in the command.".into());
    }
    if started {
        args.push(token);
    }
    Ok(args)
}

pub fn display(args: &[String]) -> String {
    std::iter::once("redumper".to_string())
        .chain(args.iter().map(|arg| {
            if arg
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_=./:+,".contains(c))
            {
                arg.clone()
            } else {
                format!("\"{}\"", arg.replace('\\', "\\\\").replace('"', "\\\""))
            }
        }))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn arguments(
    request: &DumpRequest,
    output: &Path,
    refine: bool,
) -> Result<Vec<String>, String> {
    let Some(command) = request.manual_command.as_deref() else {
        return Ok(dump_args(request, output, refine));
    };
    if request.options.double_dump && !refine {
        return Err("Turn off double dumping to use a custom command.".into());
    }
    let mut args = split(command)?;
    if args
        .first()
        .is_some_and(|arg| arg == "redumper" || arg == "redumper.exe")
    {
        args.remove(0);
    }
    if args.first().map(String::as_str) != Some("disc") {
        return Err("Use the disc command for this dumping workflow.".into());
    }
    let expected = [
        ("--drive", request.drive.clone()),
        ("--image-path", output.to_string_lossy().into_owned()),
        ("--image-name", request.name.clone()),
    ];
    let mut found = [false; 3];
    let mut result = vec!["disc".into()];
    for arg in args.into_iter().skip(1) {
        let (key, value) = arg.split_once('=').unwrap_or((&arg, ""));
        if let Some(index) = expected.iter().position(|(name, _)| *name == key) {
            let matches = value == expected[index].1
                || (key == "--image-path"
                    && Path::new(value) == Path::new(&request.output_parent).join(&request.name));
            if found[index] || !matches {
                return Err(
                    "Change the drive, image name, and output folder using the fields above."
                        .into(),
                );
            }
            found[index] = true;
            result.push(format!("{}={}", key, expected[index].1));
        } else if key == "--overwrite" || key == "--continue" {
            if !refine
                || (key == "--continue" && value != "refine")
                || (key == "--overwrite" && !value.is_empty())
            {
                return Err("Use Refine Dump to resume and overwrite an existing dump.".into());
            }
            // Reinsert exactly one pair after validation.
        } else if key == "--force-refine" || key == "--auto-eject" {
            return Err(format!(
                "{key} is managed by Disc Xplorer; use the app's controls."
            ));
        } else if !key.starts_with("--") || key.len() <= 2 {
            return Err("Use --option=value for redumper options.".into());
        } else {
            result.push(arg);
        }
    }
    if found.contains(&false) {
        return Err("Keep --drive, --image-path, and --image-name in the command.".into());
    }
    if refine {
        result.extend(["--continue=refine".into(), "--overwrite".into()]);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> DumpRequest {
        serde_json::from_value(serde_json::json!({"drive":"disk4", "drive_name":"Drive", "output_parent":"/dumps", "name":"Disc", "speed":null,"source":"internal","external_path":null})).unwrap()
    }
    #[test]
    fn round_trip_quotes_unicode_and_windows_paths_without_a_shell() {
        let args = vec![
            "disc".into(),
            "--image-path=C:\\Users\\User Name\\Disc".into(),
            "--image-name=Tom's \"Disc\" 日本 & $literal".into(),
        ];
        assert_eq!(split(&display(&args)).unwrap()[1..], args);
        assert_eq!(
            split(r#"disc '--image-path=C:\Users\Disc'"#).unwrap()[1],
            r"--image-path=C:\Users\Disc"
        );
        for bad in [
            "redumper disc && echo bad",
            "disc > file",
            "disc $(echo bad)",
            "disc `whoami`",
            "disc 'unclosed",
        ] {
            assert!(split(bad).is_err(), "{bad}");
        }
    }
    #[test]
    fn overrides_flags_but_keeps_managed_paths_and_refinement_checks() {
        let mut request = request();
        let output = Path::new("/dumps/Disc");
        let base = display(&dump_args(&request, output, false));
        request.manual_command = Some(
            base.replace("--retries=0", "--retries=12 --verbose")
                .replace(" --force-split", ""),
        );
        let args = arguments(&request, output, false).unwrap();
        assert!(args.contains(&"--retries=12".into()));
        assert!(args.contains(&"--verbose".into()));
        assert!(!args.contains(&"--force-split".into()));
        assert!(arguments(&request, output, true)
            .unwrap()
            .contains(&"--continue=refine".into()));
        for suffix in [
            " --drive=disk5",
            " --image-path=/elsewhere",
            " --overwrite",
            " --continue=split",
            " --force-refine",
            " --auto-eject",
        ] {
            request.manual_command = Some(format!("{base}{suffix}"));
            assert!(arguments(&request, output, false).is_err(), "{suffix}");
        }
        request.manual_command = Some(base.replace("disc ", "dump "));
        assert!(arguments(&request, output, false).is_err());
        request.manual_command = Some(base);
        request.options.double_dump = true;
        assert!(arguments(&request, output, false).is_err());
        assert!(arguments(&request, output, true).is_ok());
    }
}
