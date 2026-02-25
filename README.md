# tmignore-rs
Makes Time Machine respect .gitignore files
This tool is a drop-in replacement for [tmignore](https://github.com/samuelmeuli/tmignore). 
It will import the tmignore cache the first time it will launched.

## Requirements
This program runs on MacOS only and it requires Git to be installed.

## How to use it
The most important command is the `monitor` command:
```
tmignore-rs monitor
```
## Show help
```
tmignore-rs --help
```
```
Makes Time Machine respect .gitignore files

Usage: tmignore-rs <COMMAND>

Commands:
  run      Scan for paths to add or remove from the backup exclusion list
  list     Print the backup exclusion list
  reset    Reset the backup exclusion list
  monitor  Watch for file changes and keep the exclusion list up to date
  help     Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```
You can also get help about a specific command:
```
tmignore-rs monitor --help
```

## Configuration
The configuration file is located at `~/.config/tmignore/config.json`.
Here is the default configuration created automatically the first time you run tmignore-rs.
```
{
  "searchPaths": [
    "~"
  ],
  "ignoredPaths": [
    "~/.Trash",
    "~/Applications",
    "~/Downloads",
    "~/Library",
    "~/Music/iTunes",
    "~/Music/Music",
    "~/Pictures/Photos Library.photoslibrary"
  ],
  "whitelist": [],
  "threads": 4,
  "monitor_interval_secs": 5
}
```
### `searchPaths`
The list of the directories to scan.

### `ignoredPaths`
The list of directories to ignore.

### `threads`
The `threads` parameter is optional, if missing the value 0 is used. 0 means the count of threads is not limited.

### `whitelist`
The `whitelist` array expects glob-style patterns:  
 - `*.broguerec` matches all files with the `.broguerec` extension
 - `*/BrogueRunHistory.txt` matches all files named `BrogueRunHistory.txt`
See https://gitlab.com/ppentchev/fnmatch-regex-rs#overview for details.

## Profiling
There is a dedicated profile named `release-with-debug`, you can use it with:
```
cargo run --profile=release-with-debug
```
You might need to sign the binary to be able to use Instruments:
```
scripts/codesign-for-instruments.sh target/release-with-debug/tmignore-rs
```

## Tests
I had an issue using the temp folder returned by std::env::temp_dir(). This folder
is excluded from Time machine backup by default making testing impossible.