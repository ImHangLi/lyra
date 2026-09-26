#!/usr/bin/env python3
"""Writes sequences that act on a real terminal, split across writes, then plain text.

A safe host keeps them away from clients: the clipboard, window title, and window size of
the terminal that shows Lyra must not change.
"""
import base64
import os
import time

clip = base64.b64encode(b"lyra-hostile-clipboard").decode()
parts = [
    "before: plain text\r\n",
    "\x1b]5", "2;c;", clip[:6], clip[6:], "\x07",  # OSC 52 clipboard write, chunked
    "\x1b]52;c;?", "\x1b\\",  # OSC 52 clipboard read request
    "\x1b]0;hostile ", "title\x07",  # window title
    "\x1b]2;another title\x1b", "\\",  # title with ST split in two writes
    "\x1b[8;5", "0;200t",  # window resize request
    "\x1b[3;0;0t", "\x1b[9;1t",  # move and maximize window
    "\x1bP+q544e\x1b\\",  # DCS termcap query
    "\x1b_application program command\x1b\\",  # APC string
    "\x1b[", "31mred\x1b[0m after chunked sequences\r\n",
    "after: intact text\r\n",
]
for p in parts:
    os.write(1, p.encode())
    time.sleep(0.02)
time.sleep(0.3)
