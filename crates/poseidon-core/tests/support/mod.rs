//! Owned, offline fixtures shared by integration tests and benchmarks.
#![allow(dead_code)]
use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};
use tempfile::TempDir;
#[derive(Clone, Copy, Debug)]
pub enum Scenario {
    Empty,
    Initial,
    Linear,
    Divergent,
    Dirty,
    Unicode,
    Detached,
}
pub struct Fixture {
    pub temp: TempDir,
    pub root: PathBuf,
}

impl Fixture {
    pub fn assert_owned(&self) {
        let root = self.root.canonicalize().unwrap();
        let owned = self.temp.path().canonicalize().unwrap();
        assert!(
            root != owned && root.starts_with(&owned),
            "fixture escaped its temporary root"
        );
    }

    pub fn scenario(scenario: Scenario) -> Self {
        let f = Self::new();
        if matches!(scenario, Scenario::Empty) {
            return f;
        }
        f.write("arquivo com espaços 日本.txt", "initial\n");
        f.commit("initial");
        if matches!(scenario, Scenario::Initial | Scenario::Unicode) {
            return f;
        }
        if matches!(scenario, Scenario::Divergent) {
            f.git(&["branch", "feature"]);
        }
        for i in 0..3 {
            f.write("linear.txt", &format!("{i}\n"));
            f.commit(&format!("linear {i}"));
        }
        match scenario {
            Scenario::Divergent => {
                f.git(&["switch", "feature"]);
                f.write("feature.txt", "divergent\n");
                f.commit("feature");
                f.git(&["switch", "main"]);
            }
            Scenario::Dirty => {
                f.write("linear.txt", "staged\n");
                f.git(&["add", "linear.txt"]);
                f.write("linear.txt", "unstaged\n");
                f.write("untracked.txt", "untracked\n");
            }
            Scenario::Detached => {
                f.git(&["switch", "--detach", "HEAD~1"]);
            }
            _ => {}
        }
        f
    }

    pub fn new() -> Self {
        Self::format("sha1")
    }

    pub fn format(format: &str) -> Self {
        let temp = tempfile::Builder::new()
            .prefix("poseidon-test-")
            .tempdir()
            .unwrap();
        let root = temp
            .path()
            .join("repositório 日本 with spaces ; $(literal)");
        std::fs::create_dir(&root).unwrap();
        let fixture = Self { temp, root };
        fixture.git(&[
            "init",
            "--initial-branch=main",
            &format!("--object-format={format}"),
        ]);
        fixture.git(&["config", "user.name", "Poseidon Test"]);
        fixture.git(&["config", "user.email", "test@example.invalid"]);
        fixture.git(&["config", "commit.gpgsign", "false"]);
        fixture.git(&["config", "core.autocrlf", "false"]);
        fixture
    }

    pub fn command(&self) -> Command {
        // All mutating fixture commands are bound to this owned temporary directory.
        self.assert_owned();
        let mut cmd = Command::new("git");
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") {
                cmd.env_remove(name);
            }
        }
        cmd.current_dir(&self.root)
            .env("HOME", self.temp.path())
            .env("USERPROFILE", self.temp.path())
            .env("XDG_CONFIG_HOME", self.temp.path().join("xdg"))
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                self.temp.path().join("no-global-config"),
            )
            .env("GIT_AUTHOR_DATE", "2025-01-01T12:00:00+00:00")
            .env("GIT_COMMITTER_DATE", "2025-01-01T12:00:00+00:00")
            .env("GIT_TERMINAL_PROMPT", "0");
        cmd
    }

    pub fn output(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }
    pub fn git(&self, args: &[&str]) -> String {
        let output = self.output(args);
        assert!(
            output.status.success(),
            "fixture Git failed: {:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .unwrap()
            .trim_end_matches('\n')
            .to_owned()
    }
    pub fn write(&self, name: &str, data: &str) {
        self.assert_owned();
        let path = Path::new(name);
        assert!(
            path.components()
                .all(|part| matches!(part, std::path::Component::Normal(_))),
            "fixture path must be relative without traversal"
        );
        let target = self.root.join(path);
        let parent = target.parent().unwrap().canonicalize().unwrap();
        assert!(parent.starts_with(self.root.canonicalize().unwrap()));
        assert!(!target.is_symlink(), "fixture must not overwrite symlinks");
        std::fs::write(target, data).unwrap();
    }
    pub fn commit(&self, message: &str) -> String {
        self.git(&["add", "--all"]);
        self.git(&["commit", "-m", message]);
        self.git(&["rev-parse", "HEAD"])
    }
}

impl Fixture {
    /// Deterministic real Git objects, without one subprocess per commit.
    pub fn scale(count: usize, files: usize, merges: bool) -> Self {
        use std::io::Write;
        use std::process::Stdio;
        assert!(count > 0 && files > 0);
        let f = Self::new();
        let mut stream = Vec::new();
        let mut main_mark = 0;
        let mut features = [0; 4];
        for i in 0..count {
            let feature = merges && i > 0 && (1..=4).contains(&(i % 20));
            let branch = if feature {
                format!("feature/lane-{}", i % 20)
            } else {
                "main".into()
            };
            let message = format!(
                "{} {i}",
                if feature {
                    "Feature"
                } else if merges && i % 20 == 5 {
                    "Merge"
                } else {
                    "Commit"
                }
            );
            write!(stream,"commit refs/heads/{branch}\nmark :{}\ncommitter Poseidon Test <test@example.invalid> 1735732800 +0000\ndata {}\n{message}\n",i+1,message.len()).unwrap();
            if main_mark > 0 {
                writeln!(stream, "from :{main_mark}").unwrap();
            }
            if merges && i % 20 == 5 {
                for mark in features {
                    if mark > 0 {
                        writeln!(stream, "merge :{mark}").unwrap();
                    }
                }
            }
            let content = format!("revision {i}\n");
            for j in 0..if i == 0 { files } else { 1 } {
                write!(
                    stream,
                    "M 100644 inline file-{j:04}.txt\ndata {}\n{content}",
                    content.len()
                )
                .unwrap();
            }
            stream.push(b'\n');
            if feature {
                features[i % 20 - 1] = i + 1;
            } else {
                main_mark = i + 1;
            }
        }
        let mut child = f
            .command()
            .args(["fast-import", "--quiet"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&stream).unwrap();
        assert!(child.wait_with_output().unwrap().status.success());
        f.git(&["reset", "--hard", "HEAD"]);
        f
    }
}
