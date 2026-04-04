use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{self, Child, ChildStdin, Command, Output, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use time::{Date, Month, PrimitiveDateTime, Time as CivilTime, UtcOffset};

const MAIN_BRANCH: &str = "main";
const MAIN_REF: &str = "refs/heads/main";
const BOT_NAME: &str = "legalize-kr-bot";
const BOT_EMAIL: &str = "bot@legalize.kr";
const INITIAL_COMMIT_AUTHOR_NAME: &str = "Junghwan Park";
const INITIAL_COMMIT_AUTHOR_EMAIL: &str = "reserve.dev@gmail.com";
const INITIAL_COMMIT_CO_AUTHORS: &[(&str, &str)] = &[("Jihyeon Kim", "simnalamburt@gmail.com")];
const INITIAL_COMMIT_COMMITTER_NAME: &str = "Jihyeon Kim";
const INITIAL_COMMIT_COMMITTER_EMAIL: &str = "simnalamburt@gmail.com";

#[derive(Debug, Clone, Copy)]
struct GitPerson<'a> {
    name: &'a str,
    email: &'a str,
}

#[derive(Debug, Clone, Copy)]
struct FastImportTimestamp {
    epoch: i64,
    offset_minutes: i32,
}

struct CommitSpec<'a> {
    author: GitPerson<'a>,
    committer: GitPerson<'a>,
    time: FastImportTimestamp,
    message: &'a str,
    file_update: Option<(u64, &'a str)>,
}

struct FastImportProcess {
    child: Child,
    stdin: BufWriter<ChildStdin>,
}

pub struct BareRepoWriter {
    import: Option<FastImportProcess>,
    temp_output: PathBuf,
    final_output: PathBuf,
    next_mark: u64,
    parent_commit_mark: Option<u64>,
}

impl BareRepoWriter {
    pub fn create(output: &Path) -> Result<Self> {
        let final_output = output.to_path_buf();
        let temp_output = make_temp_output_path(output)?;
        if temp_output.exists() {
            remove_path(&temp_output)?;
        }

        let parent = temp_output
            .parent()
            .context("temporary output path has no parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;

        init_bare_repo(&temp_output)?;
        let mut import = FastImportProcess::spawn(&temp_output)?;
        import.write_feature_done()?;

        Ok(Self {
            import: Some(import),
            temp_output,
            final_output,
            next_mark: 1,
            parent_commit_mark: None,
        })
    }

    pub fn commit_law(
        &mut self,
        path: &str,
        markdown: &[u8],
        message: &str,
        promulgation_date: &str,
    ) -> Result<()> {
        let time = commit_time(promulgation_date)?;
        self.commit_file(
            path,
            markdown,
            message,
            GitPerson {
                name: BOT_NAME,
                email: BOT_EMAIL,
            },
            GitPerson {
                name: BOT_NAME,
                email: BOT_EMAIL,
            },
            time,
        )
    }

    pub fn commit_static(
        &mut self,
        path: &str,
        content: &[u8],
        message: &str,
        epoch: i64,
        offset_minutes: i32,
    ) -> Result<()> {
        let message = append_co_author_trailers(message, INITIAL_COMMIT_CO_AUTHORS);
        let author = GitPerson {
            name: INITIAL_COMMIT_AUTHOR_NAME,
            email: INITIAL_COMMIT_AUTHOR_EMAIL,
        };
        self.commit_file(
            path,
            content,
            &message,
            author,
            author,
            FastImportTimestamp {
                epoch,
                offset_minutes,
            },
        )
    }

    pub fn commit_empty_initial_contributor(
        &mut self,
        message: &str,
        epoch: i64,
        offset_minutes: i32,
    ) -> Result<()> {
        if self.parent_commit_mark.is_none() {
            bail!("empty contributor commit requires an existing tree");
        }
        let author = GitPerson {
            name: INITIAL_COMMIT_COMMITTER_NAME,
            email: INITIAL_COMMIT_COMMITTER_EMAIL,
        };
        self.commit_existing_tree(
            message,
            author,
            author,
            FastImportTimestamp {
                epoch,
                offset_minutes,
            },
        )
    }

    pub fn finish(mut self) -> Result<()> {
        if let Some(import) = self.import.take() {
            import.finish()?;
        }

        if self.final_output.exists() {
            remove_path(&self.final_output)?;
        }
        fs::rename(&self.temp_output, &self.final_output).with_context(|| {
            format!(
                "failed to move {} to {}",
                self.temp_output.display(),
                self.final_output.display()
            )
        })?;
        Ok(())
    }

    fn commit_file(
        &mut self,
        path: &str,
        content: &[u8],
        message: &str,
        author: GitPerson<'_>,
        committer: GitPerson<'_>,
        time: FastImportTimestamp,
    ) -> Result<()> {
        ensure_repo_path(path)?;
        let blob_mark = self.next_mark();
        let commit_mark = self.next_mark();
        let parent_commit_mark = self.parent_commit_mark;
        self.import_mut()?.write_blob(blob_mark, content)?;
        self.import_mut()?.write_commit(
            commit_mark,
            parent_commit_mark,
            CommitSpec {
                author,
                committer,
                time,
                message,
                file_update: Some((blob_mark, path)),
            },
        )?;
        self.parent_commit_mark = Some(commit_mark);
        Ok(())
    }

    fn commit_existing_tree(
        &mut self,
        message: &str,
        author: GitPerson<'_>,
        committer: GitPerson<'_>,
        time: FastImportTimestamp,
    ) -> Result<()> {
        let commit_mark = self.next_mark();
        let parent_commit_mark = self.parent_commit_mark;
        self.import_mut()?.write_commit(
            commit_mark,
            parent_commit_mark,
            CommitSpec {
                author,
                committer,
                time,
                message,
                file_update: None,
            },
        )?;
        self.parent_commit_mark = Some(commit_mark);
        Ok(())
    }

    fn import_mut(&mut self) -> Result<&mut FastImportProcess> {
        self.import
            .as_mut()
            .context("fast-import process already finalized")
    }

    fn next_mark(&mut self) -> u64 {
        let mark = self.next_mark;
        self.next_mark += 1;
        mark
    }
}

impl FastImportProcess {
    fn spawn(repo_dir: &Path) -> Result<Self> {
        let mut command = git_command();
        command
            .arg("-C")
            .arg(repo_dir)
            .arg("fast-import")
            .arg("--quiet")
            .arg("--date-format=raw-permissive")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let mut child = command.spawn().with_context(|| {
            format!("failed to spawn git fast-import for {}", repo_dir.display())
        })?;
        let stdin = child
            .stdin
            .take()
            .context("failed to open git fast-import stdin")?;

        Ok(Self {
            child,
            stdin: BufWriter::new(stdin),
        })
    }

    fn write_feature_done(&mut self) -> Result<()> {
        self.stdin.write_all(b"feature done\n")?;
        Ok(())
    }

    fn write_blob(&mut self, mark: u64, content: &[u8]) -> Result<()> {
        self.stdin.write_all(b"blob\n")?;
        writeln!(self.stdin, "mark :{mark}")?;
        write_data(&mut self.stdin, content)?;
        Ok(())
    }

    fn write_commit(
        &mut self,
        mark: u64,
        parent_mark: Option<u64>,
        spec: CommitSpec<'_>,
    ) -> Result<()> {
        writeln!(self.stdin, "commit {MAIN_REF}")?;
        writeln!(self.stdin, "mark :{mark}")?;
        write_person_line(&mut self.stdin, "author", spec.author, spec.time)?;
        write_person_line(&mut self.stdin, "committer", spec.committer, spec.time)?;
        write_data(&mut self.stdin, spec.message.as_bytes())?;
        if let Some(parent_mark) = parent_mark {
            writeln!(self.stdin, "from :{parent_mark}")?;
        }
        if let Some((blob_mark, path)) = spec.file_update {
            writeln!(self.stdin, "M 100644 :{blob_mark} {path}")?;
        }
        Ok(())
    }

    fn finish(mut self) -> Result<()> {
        self.stdin.write_all(b"done\n")?;
        self.stdin.flush()?;
        drop(self.stdin);

        let output = self
            .child
            .wait_with_output()
            .context("failed waiting for git fast-import")?;
        ensure_command_success(output, "git fast-import")
    }
}

fn make_temp_output_path(output: &Path) -> Result<PathBuf> {
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let name = output
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("invalid output path: {}", output.display()))?;
    Ok(parent.join(format!(".{name}.tmp-{}", process::id())))
}

fn init_bare_repo(repo_dir: &Path) -> Result<()> {
    let mut init_reftable = git_command();
    init_reftable
        .arg("init")
        .arg("--quiet")
        .arg("--bare")
        .arg("--initial-branch")
        .arg(MAIN_BRANCH)
        .arg("--ref-format")
        .arg("reftable")
        .arg(repo_dir);

    match init_reftable.output() {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => {
            let reftable_stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            let mut init_files = git_command();
            init_files
                .arg("init")
                .arg("--quiet")
                .arg("--bare")
                .arg("--initial-branch")
                .arg(MAIN_BRANCH)
                .arg(repo_dir);
            let output = init_files
                .output()
                .with_context(|| format!("failed to init bare repo at {}", repo_dir.display()))?;
            ensure_command_success(
                output,
                &format!(
                    "git init --bare fallback failed after reftable init error: {reftable_stderr}"
                ),
            )
        }
        Err(error) => Err(error)
            .with_context(|| format!("failed to init bare repo at {}", repo_dir.display())),
    }
}

fn git_command() -> Command {
    let mut command = Command::new("git");
    command.env("GIT_CONFIG_GLOBAL", "/dev/null");
    command.env("GIT_CONFIG_NOSYSTEM", "1");
    command.env_remove("GIT_DIR");
    command.env_remove("GIT_WORK_TREE");
    command
}

fn ensure_command_success(output: Output, context: &str) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    bail!(
        "{context}: exit status {}{}{}",
        output.status,
        if stderr.is_empty() { "" } else { "\nstderr:\n" },
        if stderr.is_empty() {
            String::new()
        } else if stdout.is_empty() {
            stderr
        } else {
            format!("{stderr}\nstdout:\n{stdout}")
        }
    )
}

fn write_person_line(
    writer: &mut impl Write,
    kind: &str,
    person: GitPerson<'_>,
    time: FastImportTimestamp,
) -> Result<()> {
    writeln!(
        writer,
        "{kind} {} <{}> {} {}",
        person.name,
        person.email,
        time.epoch,
        format_timezone_offset(time.offset_minutes)
    )?;
    Ok(())
}

fn write_data(writer: &mut impl Write, bytes: &[u8]) -> Result<()> {
    writeln!(writer, "data {}", bytes.len())?;
    writer.write_all(bytes)?;
    writer.write_all(b"\n")?;
    Ok(())
}

fn format_timezone_offset(offset_minutes: i32) -> String {
    let sign = if offset_minutes < 0 { '-' } else { '+' };
    let total_minutes = offset_minutes.abs();
    let hours = total_minutes / 60;
    let minutes = total_minutes % 60;
    format!("{sign}{hours:02}{minutes:02}")
}

fn append_co_author_trailers(message: &str, co_authors: &[(&str, &str)]) -> String {
    if co_authors.is_empty() {
        return message.to_owned();
    }

    let mut rendered = String::from(message.trim_end());
    rendered.push_str("\n\n");
    for (index, (name, email)) in co_authors.iter().enumerate() {
        if index > 0 {
            rendered.push('\n');
        }
        rendered.push_str("Co-authored-by: ");
        rendered.push_str(name);
        rendered.push_str(" <");
        rendered.push_str(email);
        rendered.push('>');
    }
    rendered
}

fn ensure_repo_path(path: &str) -> Result<()> {
    if path.split('/').find(|part| !part.is_empty()).is_none() {
        bail!("invalid empty repository path");
    }
    Ok(())
}

fn remove_path(path: &Path) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("failed to read {}", path.display()))?;
    if metadata.is_dir() {
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))?;
    } else {
        fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

fn commit_time(promulgation_date: &str) -> Result<FastImportTimestamp> {
    let effective_date = if promulgation_date.len() == 8
        && promulgation_date.bytes().all(|byte| byte.is_ascii_digit())
    {
        format!(
            "{}-{}-{}",
            &promulgation_date[..4],
            &promulgation_date[4..6],
            &promulgation_date[6..8]
        )
    } else {
        promulgation_date.to_owned()
    };

    let effective_date = if effective_date.len() != 10 {
        String::from("2000-01-01")
    } else if effective_date.as_str() < "1970-01-01" {
        String::from("1970-01-01")
    } else {
        effective_date
    };

    let year = effective_date[0..4].parse::<i32>()?;
    let month = effective_date[5..7].parse::<u8>()?;
    let day = effective_date[8..10].parse::<u8>()?;
    let month = Month::try_from(month)?;
    let date = Date::from_calendar_date(year, month, day)?;
    let datetime = PrimitiveDateTime::new(date, CivilTime::from_hms(12, 0, 0)?);
    let offset = UtcOffset::from_hms(9, 0, 0)?;
    Ok(FastImportTimestamp {
        epoch: datetime.assume_offset(offset).unix_timestamp(),
        offset_minutes: 9 * 60,
    })
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn clamps_pre_epoch_dates() {
        let temp = TempDir::new().unwrap();
        let output = temp.path().join("output.git");
        let mut writer = BareRepoWriter::create(&output).unwrap();
        writer
            .commit_law("kr/테스트법/법률.md", b"body", "message", "19491021")
            .unwrap();
        writer.finish().unwrap();

        let epoch = git_stdout(&output, ["show", "-s", "--format=%at", "HEAD"]);
        let date = git_stdout(&output, ["show", "-s", "--format=%ai", "HEAD"]);
        assert_eq!(epoch.trim(), "10800");
        assert_eq!(date.trim(), "1970-01-01 12:00:00 +0900");
    }

    fn git_stdout<const N: usize>(repo: &Path, args: [&str; N]) -> String {
        let mut command = git_command();
        command.arg("-C").arg(repo);
        for arg in args {
            command.arg(arg);
        }

        let output = command.output().unwrap();
        let stdout = output.stdout.clone();
        ensure_command_success(output, "git test helper").unwrap();
        String::from_utf8(stdout).unwrap()
    }
}
