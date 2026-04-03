use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use anyhow::{Context, Result, anyhow};
use git2::{Commit, Repository, Signature, Time as GitTime, Tree};
use time::{Date, Month, PrimitiveDateTime, Time as CivilTime, UtcOffset};

const MAIN_REF: &str = "refs/heads/main";

pub struct BareRepoWriter {
    repo: Repository,
    temp_output: PathBuf,
    final_output: PathBuf,
    parent_commit: Option<git2::Oid>,
    current_tree: Option<git2::Oid>,
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

        let repo = Repository::init_bare(&temp_output)
            .with_context(|| format!("failed to init bare repo at {}", temp_output.display()))?;
        repo.reference_symbolic("HEAD", MAIN_REF, true, "set HEAD to main")?;

        Ok(Self {
            repo,
            temp_output,
            final_output,
            parent_commit: None,
            current_tree: None,
        })
    }

    pub fn commit_law(
        &mut self,
        path: &str,
        markdown: &[u8],
        message: &str,
        promulgation_date: &str,
    ) -> Result<git2::Oid> {
        let blob_oid = self.repo.blob(markdown)?;
        let path_parts = split_path(path)?;

        let base_tree = self
            .current_tree
            .map(|oid| self.repo.find_tree(oid))
            .transpose()?;
        let tree_oid = upsert_path(&self.repo, base_tree.as_ref(), &path_parts, blob_oid)?;
        let tree = self.repo.find_tree(tree_oid)?;

        let time = commit_time(promulgation_date)?;
        const BOT_NAME: &str = "legalize-kr-bot";
        const BOT_EMAIL: &str = "bot@legalize.kr";
        let author = Signature::new(BOT_NAME, BOT_EMAIL, &time)?;
        let committer = Signature::new(BOT_NAME, BOT_EMAIL, &time)?;

        let parent_commits = self
            .parent_commit
            .map(|oid| self.repo.find_commit(oid))
            .transpose()?
            .into_iter()
            .collect::<Vec<Commit>>();
        let parent_refs = parent_commits.iter().collect::<Vec<&Commit>>();

        let commit_oid = self.repo.commit(
            Some(MAIN_REF),
            &author,
            &committer,
            message,
            &tree,
            &parent_refs,
        )?;

        self.parent_commit = Some(commit_oid);
        self.current_tree = Some(tree_oid);
        Ok(commit_oid)
    }

    pub fn commit_static(
        &mut self,
        path: &str,
        content: &[u8],
        message: &str,
        epoch: i64,
        offset_minutes: i32,
    ) -> Result<git2::Oid> {
        let blob_oid = self.repo.blob(content)?;
        let path_parts = split_path(path)?;

        let base_tree = self
            .current_tree
            .map(|oid| self.repo.find_tree(oid))
            .transpose()?;
        let tree_oid = upsert_path(&self.repo, base_tree.as_ref(), &path_parts, blob_oid)?;
        let tree = self.repo.find_tree(tree_oid)?;

        let time = GitTime::new(epoch, offset_minutes);
        const INITIAL_COMMIT_AUTHOR_NAME: &str = "Junghwan Park";
        const INITIAL_COMMIT_AUTHOR_EMAIL: &str = "reserve.dev@gmail.com";
        const INITIAL_COMMIT_CO_AUTHORS: &[(&str, &str)] =
            &[("Jihyeon Kim", "simnalamburt@gmail.com")];
        let author = Signature::new(
            INITIAL_COMMIT_AUTHOR_NAME,
            INITIAL_COMMIT_AUTHOR_EMAIL,
            &time,
        )?;
        let committer = Signature::new(
            INITIAL_COMMIT_AUTHOR_NAME,
            INITIAL_COMMIT_AUTHOR_EMAIL,
            &time,
        )?;
        let message = append_co_author_trailers(message, INITIAL_COMMIT_CO_AUTHORS);

        let parent_commits = self
            .parent_commit
            .map(|oid| self.repo.find_commit(oid))
            .transpose()?
            .into_iter()
            .collect::<Vec<Commit>>();
        let parent_refs = parent_commits.iter().collect::<Vec<&Commit>>();

        let commit_oid = self.repo.commit(
            Some(MAIN_REF),
            &author,
            &committer,
            &message,
            &tree,
            &parent_refs,
        )?;

        self.parent_commit = Some(commit_oid);
        self.current_tree = Some(tree_oid);
        Ok(commit_oid)
    }

    pub fn commit_empty_initial_contributor(
        &mut self,
        message: &str,
        epoch: i64,
        offset_minutes: i32,
    ) -> Result<git2::Oid> {
        let tree_oid = self
            .current_tree
            .context("empty contributor commit requires an existing tree")?;
        let tree = self.repo.find_tree(tree_oid)?;

        let time = GitTime::new(epoch, offset_minutes);
        const INITIAL_COMMIT_COMMITTER_NAME: &str = "Jihyeon Kim";
        const INITIAL_COMMIT_COMMITTER_EMAIL: &str = "simnalamburt@gmail.com";
        let author = Signature::new(
            INITIAL_COMMIT_COMMITTER_NAME,
            INITIAL_COMMIT_COMMITTER_EMAIL,
            &time,
        )?;
        let committer = Signature::new(
            INITIAL_COMMIT_COMMITTER_NAME,
            INITIAL_COMMIT_COMMITTER_EMAIL,
            &time,
        )?;

        let parent_commits = self
            .parent_commit
            .map(|oid| self.repo.find_commit(oid))
            .transpose()?
            .into_iter()
            .collect::<Vec<Commit>>();
        let parent_refs = parent_commits.iter().collect::<Vec<&Commit>>();

        let commit_oid = self.repo.commit(
            Some(MAIN_REF),
            &author,
            &committer,
            message,
            &tree,
            &parent_refs,
        )?;

        self.parent_commit = Some(commit_oid);
        self.current_tree = Some(tree_oid);
        Ok(commit_oid)
    }

    pub fn finish(self) -> Result<()> {
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

fn make_temp_output_path(output: &Path) -> Result<PathBuf> {
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let name = output
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("invalid output path: {}", output.display()))?;
    Ok(parent.join(format!(".{name}.tmp-{}", process::id())))
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

fn split_path(path: &str) -> Result<Vec<&str>> {
    let parts = path
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return Err(anyhow!("invalid empty repository path"));
    }
    Ok(parts)
}

fn upsert_path(
    repo: &Repository,
    base_tree: Option<&Tree<'_>>,
    path_parts: &[&str],
    blob_oid: git2::Oid,
) -> Result<git2::Oid> {
    let mut builder = repo.treebuilder(base_tree)?;
    if path_parts.len() == 1 {
        builder.insert(path_parts[0], blob_oid, 0o100644)?;
        return Ok(builder.write()?);
    }

    let next_tree = if let Some(tree) = base_tree {
        if let Some(entry) = tree.get_name(path_parts[0]) {
            if entry.kind() == Some(git2::ObjectType::Tree) {
                Some(repo.find_tree(entry.id())?)
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    let child_tree_oid = upsert_path(repo, next_tree.as_ref(), &path_parts[1..], blob_oid)?;
    builder.insert(path_parts[0], child_tree_oid, 0o040000)?;
    Ok(builder.write()?)
}

fn commit_time(promulgation_date: &str) -> Result<GitTime> {
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
    let timestamp = datetime.assume_offset(offset).unix_timestamp();
    const KST_OFFSET_MINUTES: i32 = 9 * 60;
    Ok(GitTime::new(timestamp, KST_OFFSET_MINUTES))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_pre_epoch_dates() {
        let time = commit_time("19491021").unwrap();
        assert_eq!(time.seconds(), 12 * 60 * 60 - 9 * 60 * 60);
        assert_eq!(time.offset_minutes(), 540);
    }
}
