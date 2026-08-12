#!/usr/bin/env python3
"""Compare privacy-masked byte transcripts without decoding their schemas."""

import argparse
import hashlib
import json


def first_difference(left: bytes, right: bytes) -> str:
    for offset, (a, b) in enumerate(zip(left, right)):
        if a != b:
            bits = a ^ b
            return f"byte={offset} bit={next(i for i in range(8) if bits & (1 << i))} linux={a:02x} userspace={b:02x}"
    if len(left) != len(right):
        return f"byte={min(len(left), len(right))} bit=length linux_len={len(left)} userspace_len={len(right)}"
    return "none"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("manifest", help="JSON object of name -> {linux, userspace} masked hex")
    args = parser.parse_args()
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
