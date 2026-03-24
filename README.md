# tmignore-rs [![codecov](https://codecov.io/gh/IohannRabeson/tmignore-rs/graph/badge.svg?token=B5Q69GVFGN)](https://codecov.io/gh/IohannRabeson/tmignore-rs)
Makes Time Machine respect .gitignore files.  
This tool is a drop-in replacement for [tmignore](https://github.com/samuelmeuli/tmignore) but with
a brand new command 'monitor' that is updating in (almost) real time the cache if changes in the filesystem are detected.

It will happilly import the tmignore cache and configuration the first time it will launched.

Compared to tmignore it should be very fast, where tmignore was taking minutes it's now few seconds.

## Requirements
This program runs on MacOS only and it requires Git to be installed.

## Installation
The easiest is to use Homebrew:
```
brew tap iohannrabeson/tap
brew install tmignore-rs
brew services start tmignore-rs
```
You have to do that only once, tmignore-rs will be started automatically on startup.

You can stop the service using:
```
brew services stop tmignore-rs
```

## How to use it
### `monitor` command
The most important command is the `monitor` command:
```
tmignore-rs monitor
```
It will monitor the filesystem and update the list of paths to exclude from Time Machine backups every 60 seconds.

This program loads `~/.config/tmignore-rs/config.json` as its configuration file, creating it on first run if it doesn't exist.  
This configuration file is hot-reloaded so you don't need to restart tmignore-rs after modifying it.
See [Configuration](#configuration) for more details.
 
This command is very light, excepted the initial scan, it should never affect the performances of you Mac.
If you want to test you can run it with the flag `--dry-run` to prevent avoid modifying anything.  
But for testing, it's easier to use the `run` command.

### `run` command
This command performs a scan of the directories. You can specify the number of threads to use during this phase, no need to set it high you will be limited by the I/O anyways.  
Like `monitor`, the `run` command has an option `--dry-run`.
If you want to run tmignore-rs manually times to times this is the command to use.

### `reset` command
This command removes everything from the backup exclusion list.

### `list` command
This command prints the backup exclusion list.

There is a `-0` option if you want a null separated list.

## Logs
This application sends the logs to the Console application.  
Use `tmignore-rs` as filter (select the filtering by process).  

## Show help
```
tmignore-rs --help
```
```
Makes Time Machine respect .gitignore files

Usage: tmignore-rs [OPTIONS] <COMMAND>

Commands:
  monitor  Watch for file changes and keep the exclusion list up to date
  run      Scan for paths to add or remove from the backup exclusion list
  list     Print the backup exclusion list
  reset    Reset the backup exclusion list
  help     Print this message or the help of the given subcommand(s)

Options:
  -v, --verbose  Enable verbose logging
  -h, --help     Print help
  -V, --version  Print version

```
You can also get help about a specific command:
```
tmignore-rs monitor --help
```

## Configuration
The configuration file is located at `~/.config/tmignore-rs/config.json`.
Here is the default configuration created automatically the first time you run tmignore-rs.   
If you were using [tmignore](https://github.com/samuelmeuli/tmignore) the configuration will be imported.

```
{
  "search_directories": [
    "~"
  ],
  "ignored_directories": [
    "~/.Trash",
    "~/Applications",
    "~/Downloads",
    "~/Library",
    "~/Music/Music",
    "~/Music/iTunes",
    "~/Pictures/Photos Library.photoslibrary"
  ],
  "whitelist_patterns": [
    "*.broguerec",
    "*.broguesave",
    "*/BrogueHighScores.txt",
    "*/BrogueRunHistory.txt"
  ],
  "threads": 4,
  "monitor_interval_secs": 60
}

```
### `search_directories`
The list of the directories to scan.

### `ignored_directories`
The list of directories to ignore.

### `whitelist_patterns`
The list of entries that should always be included in backup.
The `whitelist_patterns` array expects glob-style patterns:  
 - `*.broguerec` matches all files with the `.broguerec` extension
 - `*/BrogueRunHistory.txt` matches all files named `BrogueRunHistory.txt`
See https://gitlab.com/ppentchev/fnmatch-regex-rs#overview for details.

### `threads`
The `threads` count. 0 means the count of threads is not limited and the max will be choose.

### `monitor_interval_secs`
Monitoring interval in seconds. Default is 60 seconds. If you want to reduce the power consumption increase the interval.

## Coverage
I'm using [Tarpaulin](https://github.com/xd009642/tarpaulin) to measure test coverage.
When developing run tarpaulin before doing changes, then run it with your changes and tarpaulin will tell you how the coverage progressed.

![Coverage chart](https://codecov.io/gh/IohannRabeson/tmignore-rs/graphs/tree.svg?token=B5Q69GVFGN)
