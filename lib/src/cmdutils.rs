use std::{
    io::{Read, Seek},
    process::Command,
};

use anyhow::{Context, Result};

/// Helpers intended for [`std::process::Command`].
pub(crate) trait CommandRunExt {
    fn run(&mut self) -> Result<()>;
    /// Execute the child process, parsing its stdout as JSON.
    fn run_and_parse_json<T: serde::de::DeserializeOwned>(&mut self) -> Result<T>;
}

/// Helpers intended for [`std::process::ExitStatus`].
pub(crate) trait ExitStatusExt {
    /// If the exit status signals it was not successful, return an error.
    /// Note that we intentionally *don't* include the command string
    /// in the output; we leave it to the caller to add that if they want,
    /// as it may be verbose.
    fn check_status(&mut self, stderr: std::fs::File) -> Result<()>;
}

/// Parse the last chunk (e.g. 1024 bytes) from the provided file,
/// ensure it's UTF-8, and return that value. This function is infallible;
/// if the file cannot be read for some reason, a copy of a static string
/// is returned.
fn last_utf8_content_from_file(mut f: std::fs::File) -> String {
    // u16 since we truncate to just the trailing bytes here
    // to avoid pathological error messages
    const MAX_STDERR_BYTES: u16 = 1024;
    let size = f
        .metadata()
        .map_err(|e| {
            tracing::warn!("failed to fstat: {e}");
        })
        .map(|m| m.len().try_into().unwrap_or(u16::MAX))
        .unwrap_or(0);
    let size = size.min(MAX_STDERR_BYTES);
    let seek_offset = -(size as i32);
    let mut stderr_buf = Vec::with_capacity(size.into());
    // We should never fail to seek()+read() really, but let's be conservative
    let r = match f
        .seek(std::io::SeekFrom::End(seek_offset.into()))
        .and_then(|_| f.read_to_end(&mut stderr_buf))
    {
        Ok(_) => String::from_utf8_lossy(&stderr_buf),
        Err(e) => {
            tracing::warn!("failed seek+read: {e}");
            "<failed to read stderr>".into()
        }
    };
    (&*r).to_owned()
}

impl ExitStatusExt for std::process::ExitStatus {
    fn check_status(&mut self, stderr: std::fs::File) -> Result<()> {
        let stderr_buf = last_utf8_content_from_file(stderr);
        if self.success() {
            return Ok(());
        }
        anyhow::bail!(format!("Subprocess failed: {self:?}\n{stderr_buf}"))
    }
}

impl CommandRunExt for Command {
    /// Synchronously execute the child, and return an error if the child exited unsuccessfully.
    fn run(&mut self) -> Result<()> {
        let stderr = tempfile::tempfile()?;
        self.stderr(stderr.try_clone()?);
        self.status()?.check_status(stderr)
    }

    fn run_and_parse_json<T: serde::de::DeserializeOwned>(&mut self) -> Result<T> {
        let mut stdout = tempfile::tempfile()?;
        self.stdout(stdout.try_clone()?);
        self.run()?;
        stdout.seek(std::io::SeekFrom::Start(0)).context("seek")?;
        let stdout = std::io::BufReader::new(stdout);
        serde_json::from_reader(stdout).map_err(Into::into)
    }
}

/// Helpers intended for [`tokio::process::Command`].
#[allow(dead_code)]
pub(crate) trait AsyncCommandRunExt {
    async fn run(&mut self) -> Result<()>;
}

impl AsyncCommandRunExt for tokio::process::Command {
    /// Asynchronously execute the child, and return an error if the child exited unsuccessfully.
    ///
    async fn run(&mut self) -> Result<()> {
        let stderr = tempfile::tempfile()?;
        self.stderr(stderr.try_clone()?);
        self.status().await?.check_status(stderr)
    }
}

#[test]
fn command_run_ext() {
    // The basics
    Command::new("true").run().unwrap();
    assert!(Command::new("false").run().is_err());

    // Verify we capture stderr
    let e = Command::new("/bin/sh")
        .args(["-c", "echo expected-this-oops-message 1>&2; exit 1"])
        .run()
        .err()
        .unwrap();
    similar_asserts::assert_eq!(
        e.to_string(),
        "Subprocess failed: ExitStatus(unix_wait_status(256))\nexpected-this-oops-message\n"
    );

    // Ignoring invalid UTF-8
    let e = Command::new("/bin/sh")
        .args([
            "-c",
            r"echo -e 'expected\xf5\x80\x80\x80\x80-foo\xc0bar\xc0\xc0' 1>&2; exit 1",
        ])
        .run()
        .err()
        .unwrap();
    similar_asserts::assert_eq!(
        e.to_string(),
        "Subprocess failed: ExitStatus(unix_wait_status(256))\nexpected�����-foo�bar��\n"
    );
}

#[test]
fn command_run_ext_json() {
    #[derive(serde::Deserialize)]
    struct Foo {
        a: String,
        b: u32,
    }
    let v: Foo = Command::new("echo")
        .arg(r##"{"a": "somevalue", "b": 42}"##)
        .run_and_parse_json()
        .unwrap();
    assert_eq!(v.a, "somevalue");
    assert_eq!(v.b, 42);
}

#[tokio::test]
async fn async_command_run_ext() {
    use tokio::process::Command as AsyncCommand;
    let mut success = AsyncCommand::new("true");
    let mut fail = AsyncCommand::new("false");
    // Run these in parallel just because we can
    let (success, fail) = tokio::join!(success.run(), fail.run(),);
    success.unwrap();
    assert!(fail.is_err());
}
