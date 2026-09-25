#!/usr/bin/env python3
"""Playground full-screen program: draws on the alternate screen until q is pressed."""
import curses


def main(win):
    curses.curs_set(0)
    keys = 0
    while True:
        win.erase()
        h, w = win.getmaxyx()
        win.border()
        win.addstr(1, 2, "Lyra full-screen demo"[: w - 4], curses.A_BOLD)
        win.addstr(3, 2, f"size {w}x{h}, keys pressed: {keys}"[: w - 4])
        win.addstr(5, 2, "press q to quit"[: w - 4], curses.A_REVERSE)
        win.refresh()
        ch = win.getch()
        if ch in (ord("q"), ord("Q")):
            break
        keys += 1


curses.wrapper(main)
print("fullscreen finished")
