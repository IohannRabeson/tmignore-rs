# Investigating why tmutil is so slow

I did this investigation on MacOS 15.7.4 (24G517).
```
> shasum -a 256 /usr/bin/tmutil 
9b83bbe8c308d44986890cc10e74f974d330cc4d84024862a680874457a847f2  /usr/bin/tmutil
```

```
$> otool -tv /usr/bin/tmutil | grep -i  -e exclude -e exclusion
```
With `otool`, `-t` makes it print the content of the `(__TEXT,__text)` section (where the executable code is stored) and `-v` to disassemble the text so we can read the name of the functions called.
```
❯ otool -tv /usr/bin/tmutil | grep -i -e exclude -e exclusion
000000010000eb6c	add	x1, x1, #0x5aa ; literal pool for: "%s: Cannot configure path-based exclusion for relative path.\n"
000000010000ec00	ldr	x0, [x8, #0x950] ; literal pool symbol address: _OBJC_CLASS_$_TMPathExclusion
000000010000ec30	bl	0x10002d41c ; symbol stub for: _CSBackupSetItemExcluded
000000010000f3b0	add	x9, x9, #0x660 ; literal pool for: "Excluded"
0000000100018260	add	x0, x0, #0x9c3 ; literal pool for: "Calculating space used by APFS snapshots of standard excluded items...\n"
00000001000185b0	add	x1, x1, #0xa69 ; literal pool for: "==\n%s total used by all exclusions.\n==\n"
```
I was looking for anything related to the exclusion mechanism. The symbol `_CSBackupSetItemExcluded` looked interesting and after checking on the web I found the Apple documentation for this function. It was exactly what I was looking for. I was unable to use `lldb` to help me follow the code execution because Apple hardened `tmutil`. \
So I did some backups to test and confirmed `CSBackupSetItemExcluded` was working correctly. It was much faster than calling `tmutil`, but I did not know why exactly. 

Since I was unable to use `lldb`, I had to follow the assembly code.

The return of `_CSBackupSetItemExcluded` will be stored into `x0`, and it will be `0` if it's a success.
`x0` and `w0` are 2 different registers, and `w0` is the lower 32 bits of `x0`.
```
000000010000ec30	bl	0x10002d41c ; symbol stub for: _CSBackupSetItemExcluded
000000010000ec34	mov	x21, x0 ; Store the value returned into x21
000000010000ec38	cmn	w0, #0x3d ; Set the Z flag if w0 is -61, note w0 is the lower 32 bits of x0 so it's 0 so we don't branch
000000010000ec3c	b.eq	0x10000ee28 ; branch if Z flag is raised, w0 stores 0 so we don't branch
000000010000ec40	cbnz	w21, 0x10000ee34 ; compare and jump to 0x10000ee34 if w21 is not equal 0. w21 stores 0 so we don't branch
000000010000ec44	mov	x22, #0x0 ; Store 0 into x22
000000010000ec48	b	0x10000ee84 ; Jump to 0x10000ee84
[...]
000000010000ee84	mov	w0, #0x1 ; Store 1 into w0, this register will be used as parameter for the next function call
000000010000ee88	bl	0x10002d9fc ; symbol stub for: _sleep
```
So we can see that if `CSBackupSetItemExcluded` returns `0` the program calls `sleep` with `1` as parameter, which is asking the program to wait 1 second.
Just after sleeping the program calls `__MDPerfCreateFileIndexingMarker` which is a private function, and there is no documentation about it. The only [reference](https://github.com/216k155/MacOSX-SDKs/blob/master/MacOSX10.11.sdk/System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/Metadata.framework/Versions/A/Metadata.tbd) I found suggests this function is part of `Metadata.framework` which is related to Spotlight.

What is very disappointing is that `tmutil addexclusion` does not process all paths first and then sleep once. It sleeps after each path — which explains the 14 minutes for 667 paths.
I also followed the 2 error paths (the two branches we assumed were not taken because `CSBackupSetItemExcluded` returned 0), and both ultimately also jump to `0x10000ee84`, sleeping 1 second as well. I'm not sure, if it's because they did not care about it or if it's an intentional limitation.