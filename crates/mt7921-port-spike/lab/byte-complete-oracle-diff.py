#!/usr/bin/env python3
"""Compare privacy-masked byte transcripts without decoding their schemas."""

import argparse
import hashlib
import json
import re


def first_difference(left: bytes, right: bytes) -> str:
    for offset, (a, b) in enumerate(zip(left, right)):
        if a != b:
            bits = a ^ b
            return f"byte={offset} bit={next(i for i in range(8) if bits & (1 << i))} linux={a:02x} userspace={b:02x}"
    if len(left) != len(right):
        return f"byte={min(len(left), len(right))} bit=length linux_len={len(left)} userspace_len={len(right)}"
    return "none"



FW_STATE = re.compile(r"fw_state region=(\S+) offset=(0x[0-9a-f]+) addr=(0x[0-9a-f]+) value=([0-9a-f]{8})")

def firmware_state(path, userspace):
    records, started = [], not userspace
    for line in open(path, encoding="utf-8"):
        if userspace and "fw_state_begin " in line:
            if started:
                break
            started = True
            continue
        match = FW_STATE.search(line) if started else None
        if match:
            region, offset, address, value = match.groups()
            records.append((region, int(offset, 16), int(address, 16), int(value, 16)))
    return records

def firmware_state_difference(linux_path, userspace_path):
    left = firmware_state(linux_path, False)
    right = firmware_state(userspace_path, True)
    if len(left) != 2944 or len(right) != 2944:
        return f"count={len(left)}/{len(right)} expected=2944"
    for linux, userspace in zip(left, right):
        mask = 0xfff if linux[:2] == ("wtbl_peer1", 0x8) else 0
        if linux[:3] != userspace[:3] or (linux[3] & ~mask) != (userspace[3] & ~mask):
            return (f"region={linux[0]} offset={linux[1]:#x} addr={linux[2]:#x} "
                    f"linux={linux[3]:08x} userspace={userspace[3]:08x}")
    return "none"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("manifest", nargs="?", help="JSON object of name -> {linux, userspace} masked hex")
    parser.add_argument("--firmware-state", nargs=2, metavar=("LINUX", "USERSPACE"))
    args = parser.parse_args()
    if args.firmware_state:
        difference = firmware_state_difference(*args.firmware_state)
        print(f"firmware_state first_difference={difference}")
        raise SystemExit(difference != "none")
    if not args.manifest:
        parser.error("manifest or --firmware-state is required")
    with open(args.manifest, encoding="utf-8") as source:
        records = json.load(source)

    failed = False
    for name, record in records.items():
        left = bytes.fromhex(record["linux"])
        right = bytes.fromhex(record["userspace"])
        difference = first_difference(left, right)
        failed |= difference != "none"
        print(
            f"{name}\tbytes={len(left)}/{len(right)}"
            f"\tlinux_sha256={hashlib.sha256(left).hexdigest()}"
            f"\tuserspace_sha256={hashlib.sha256(right).hexdigest()}"
            f"\tfirst_difference={difference}"
        )
    raise SystemExit(failed)


if __name__ == "__main__":
    main()
