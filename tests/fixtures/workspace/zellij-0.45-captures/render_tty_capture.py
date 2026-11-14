#!/usr/bin/env python3
"""Render a script(1) ANSI terminal recording as a fixed text viewport."""

from __future__ import annotations

import argparse
import unicodedata
from pathlib import Path


def cell_width(char: str) -> int:
    if unicodedata.combining(char):
        return 0
    return 2 if unicodedata.east_asian_width(char) in {"W", "F"} else 1


def render(data: str, rows: int, columns: int) -> str:
    screen = [[" "] * columns for _ in range(rows)]
    row = col = 0
    i = 0
    while i < len(data):
        char = data[i]
        if char == "\x1b":
            if data.startswith("\x1b[", i):
                end = i + 2
                while end < len(data) and not ("@" <= data[end] <= "~"):
                    end += 1
                if end == len(data):
                    break
                command = data[i + 2 : end]
                final = data[end]
                params = command.lstrip("?").split(";")
                nums = [int(value) if value.isdigit() else 0 for value in params]
                if final in {"H", "f"}:
                    row = max(0, (nums[0] if nums else 1) - 1)
                    col = max(0, (nums[1] if len(nums) > 1 else 1) - 1)
                elif final == "G":
                    col = max(0, (nums[0] if nums else 1) - 1)
                elif final == "d":
                    row = max(0, (nums[0] if nums else 1) - 1)
                elif final in {"A", "B", "C", "D"}:
                    amount = nums[0] if nums and nums[0] else 1
                    if final == "A":
                        row -= amount
                    elif final == "B":
                        row += amount
                    elif final == "C":
                        col += amount
                    else:
                        col -= amount
                elif final == "J" and nums and nums[0] == 2:
                    screen = [[" "] * columns for _ in range(rows)]
                elif final == "K":
                    mode = nums[0] if nums else 0
                    left, right = (0, col) if mode == 1 else (col, columns)
                    if mode == 2:
                        left, right = 0, columns
                    if 0 <= row < rows:
                        screen[row][max(0, left) : min(columns, right)] = [" "] * max(
                            0, min(columns, right) - max(0, left)
                        )
                i = end + 1
                continue
            if data.startswith("\x1b]", i):
                end = i + 2
                while end < len(data):
                    if data[end] == "\x07":
                        end += 1
                        break
                    if data.startswith("\x1b\\", end):
                        end += 2
                        break
                    end += 1
                i = end
                continue
            i += 2
            continue
        if char == "\r":
            col = 0
        elif char == "\n":
            row += 1
        elif char == "\b":
            col = max(0, col - 1)
        elif char >= " " and char != "\x7f":
            width = cell_width(char)
            if 0 <= row < rows and 0 <= col < columns:
                screen[row][col] = char
                if width == 2 and col + 1 < columns:
                    screen[row][col + 1] = " "
            col += width
            if col >= columns:
                col = columns - 1
        i += 1
    return "\n".join("".join(line).rstrip() for line in screen) + "\n"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("recording", type=Path)
    parser.add_argument("rows", type=int)
    parser.add_argument("columns", type=int)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    data = args.recording.read_bytes().decode("utf-8", errors="replace")
    args.output.write_text(render(data, args.rows, args.columns))


if __name__ == "__main__":
    main()
