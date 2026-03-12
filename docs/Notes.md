# Profiling
There is a dedicated profile named `release-with-debug`, you can use it with:
```
cargo run --profile=release-with-debug
```
You might need to sign the binary to be able to use Instruments:
```
scripts/codesign-for-instruments.sh target/release-with-debug/tmignore-rs
```

# Tests
I had an issue using the temp folder returned by std::env::temp_dir().  
This folder is excluded from Time machine backup by default making some testing impossible.

# How to delete Time Machine backups
In System Settings > Time Machine, you can remove the backup disk.
It will ask if you want to forget it click Yes.
Then you now need to erase the backup partition with Disk Utility.
Select the partition, right click > Erase...
You must be careful to start Disk Utils after mounting the backup volume otherwise Erasing will fail
with an error saying you need more privileges.

# How to browse a backup
First you need to list the backup with `tmutil listbackups`
Then you can just open the path it returns `open <path to your backup>`
This will open a Finder window and you will be able to just browse the files and folder as usual.