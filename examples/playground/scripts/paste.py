#!/usr/bin/env python3
"""Enables bracketed paste, reads one paste in raw mode, and reports what arrived."""
import os
import select
import sys
import termios
import tty

START, END = b"\x1b[200~", b"\x1b[201~"
fd = sys.stdin.fileno()
saved = termios.tcgetattr(fd)
os.write(1, b"\x1b[?2004hwaiting for one paste\r\n")
tty.setraw(fd)
buf = b""
try:
    while END not in buf:
        buf += os.read(fd, 4096)
    while select.select([fd], [], [], 0.7)[0]:
        buf += os.read(fd, 4096)
finally:
    termios.tcsetattr(fd, termios.TCSADRAIN, saved)
    os.write(1, b"\x1b[?2004l")
begin, end = buf.find(START), buf.find(END)
body = buf[begin + len(START):end] if begin >= 0 else b""
print(f"pastes={buf.count(START)} body={body.decode('utf-8', 'replace')!r}")
print(f"before_paste={buf[:max(begin, 0)]!r} after_paste={buf[end + len(END):]!r}")
