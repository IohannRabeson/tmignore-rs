# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.2](https://github.com/IohannRabeson/tmignore-rs/releases/tag/v0.1.2) - 2026-03-17

### Other

- Add a github action to update the Homebrew formula.
- v0.1.2
- Enable release-plz
- Bump actions/checkout from 5 to 6
- Update also github actions using dependabot.
- fnmatch-regex has been released on crates.io
- Update libc.
- Update rusqlite
- Update clap
- Prevent to publish on crates.io
- When monitoring, if the config reloading fails, do not stop anymore.
- README
- Log breaking error.
- Adding log
- README
- README
- Update README.md
- README
- Switch to log and send log to MacOS Console application.
- Embed the commit info when building.
- Remove debug log.
- Doc
- Format and doc.
- Add original tmignore license.
- Doc
- Now use CSBackupSetItemExcluded instead of calling tmutil.
- Update README.md
- Format
- Refactored legacy cache import in a similar way I implemented it for the legacy config.
- Decouple legacy config from the actual config.
- Reimplementation to now use tmutil to add and remove exclusions.
- Doc
- Notes
- Bump shellexpand from 3.1.1 to 3.1.2
- Bump clap from 4.5.59 to 4.5.60
- Remove log
- Make it clear a case is unreachable. I tried to implement a test and discovered that was just impossible.
- Add a test where I add a watched directory that does not exist.
- Clippy
- Reimplements monitor and now testing using a mock.
- Add notes.
- Add a test for add_exclusion
- Add codecov info.
- Trying to make test_monitor_add_a_repository less flaky.
- Trying to make test_monitor_removed_file less flaky.
- Fixed a bug where it's possible a repository outside the search directories be listed.
- .gitignore
- Documentation.
- Doc
- Implement the option -0 for the list command as requested by some guy on the original tmignore repository
- Testing list command.
- Fix issue where it's not clear the dry mode is enabled when importing the legacy cache.
- Increase a little bit the coverage by enabling detailled log.
- Format
- Cleanup new tests.
- Also test the error paths of apply_diff_and_print.
- Fix a bug where an event is accepted but the scan of repositories doesn't start if the monitoring is stopped.
- Add test for create_whitelist.
- More tests.
- More tests.
- Ensure to delete previous directory if exist.
- More tests.
- Split main.rs
- Enable coverage with Codecov.
- Remove dirs dependency.
- Fmt
- Make expand_paths private.
- Add a test for the legacy cache
- Fix README
- Documentation.
- Documentation.
- Added tests for run command.
- Canonicalize the directory path passed to Git.
- Add configuration to debug the reset command.
- Cleanup
- Sqlite cache.
- Add a script found on the internet to codesign to allow Instruments to profile the binary.
- Smarter scanning strategy when monitoring.
- Documentation.
- Remove useless usage of format!
- Fix ValidationError Display implementation.
- Added monitor_interval field to Config.
- Documentation.
- Be more strict on the filesystem events accepted when monitoring.
- Add profile `release-with-debug`
- Documentation.
- Improved display when dry run is enabled.
- All the commands that can alter the cache have now a dry_run option and a details option.
- Prevent to alter the cache when dry_run is true.
- Refactored Config and implement tests.
- Fix issue when reloading where the paths are not expanded.
- Add monitor command.
- Allow to limit the count of threads used.
- Using the exact same extended attribute value than tmutil.
- Use the original repository, the guy just closed my MR and submitted the exact same changes himself.. contributing is great sometimes ::)))
- It does not seem pratical to build on macos-intel.
- Apply clippy fixes.
- CI build using MacOS.
- Print when creating the configuration file.
- Implement whitelist.
- Documentation.
- Implement list and reset commands.
- Now the run command is effectively add/remove time machine exclusions.
- List files to ignores.
- Support for ignored directories.
- Bump clap from 4.5.58 to 4.5.59
- Create dependabot.yml
- Make it fully async
- Use less memory.
- Use parallel version of ignore for a huge performance boost (in my test it passes from 18s to 3-4s)
- Implement find_repositories.
- Setup CI
- Setup skeleton of the application.
- Initial commit
