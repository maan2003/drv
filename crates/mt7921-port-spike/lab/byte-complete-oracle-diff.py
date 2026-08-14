#!/usr/bin/env python3
"""Compare privacy-masked byte transcripts without decoding their schemas."""

import argparse
import difflib
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
MCU_SOURCE = re.compile(
    r"mcu_source cmd=(0x[0-9a-f]+) payload_len=([0-9]+) wait=([01])"
)
NATIVE_ASSOC = re.compile(r"kind=txwi .*frame_len=([0-9]+) fc=0x0000 .*payload_hash=omitted")
USERSPACE_ASSOC = re.compile(r"association_request_structure .*ie_id_lengths=([^ ]+)")


def association_source_categories(native_path, userspace_path):
    native_len = None
    with open(native_path, encoding="utf-8") as source:
        for line in source:
            match = NATIVE_ASSOC.search(line)
            if match:
                native_len = int(match.group(1))
                break
    userspace_categories = None
    with open(userspace_path, encoding="utf-8") as source:
        for line in source:
            match = USERSPACE_ASSOC.search(line)
            if match:
                userspace_categories = match.group(1).split(",")
                break
    if native_len is None or userspace_categories is None:
        raise ValueError("association source records not found")
    userspace_len = 28 + sum(2 + int(item.split(":", 1)[1]) for item in userspace_categories)
    return {
        "comparison": "source-category-only",
        "native": {
            "mpdu_length": native_len,
            "ie_categories": "unavailable",
            "exact_bytes": "unavailable-payload-hash-omitted",
        },
        "userspace": {
            "mpdu_length": userspace_len,
            "ie_categories": userspace_categories,
        },
        "length_delta": native_len - userspace_len,
        "invented_native_bytes": False,
    }


def command_sequence(path):
    records = []
    with open(path, encoding="utf-8") as source:
        for line in source:
            match = MCU_SOURCE.search(line)
            if match:
                command, payload_len, wait = match.groups()
                records.append((int(command, 16), int(payload_len), int(wait)))
    return records


def command_label(record):
    command, payload_len, wait = record
    return f"cmd={command:#x},payload_len={payload_len},wait={wait}"


def command_sequence_differences(left, right):
    differences = []
    matcher = difflib.SequenceMatcher(a=left, b=right, autojunk=False)
    for operation, left_start, left_end, right_start, right_end in matcher.get_opcodes():
        if operation == "equal":
            continue
        linux = ",".join(command_label(record) for record in left[left_start:left_end]) or "-"
        userspace = ",".join(command_label(record) for record in right[right_start:right_end]) or "-"
        differences.append(
            f"{operation} linux[{left_start}:{left_end}]={linux} "
            f"userspace[{right_start}:{right_end}]={userspace}"
        )
    return differences


def firmware_state(path, userspace):
    records, started = [], not userspace
    with open(path, encoding="utf-8") as source:
        for line in source:
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
    parser.add_argument(
        "manifest",
        nargs="?",
        help="JSON object containing commands and records; see --help",
    )
    parser.add_argument("--firmware-state", nargs=2, metavar=("LINUX", "USERSPACE"))
    parser.add_argument("--command-sequence", nargs=2, metavar=("LINUX", "USERSPACE"))
    parser.add_argument("--association-source-categories", nargs=2,
                        metavar=("NATIVE", "USERSPACE"))
    args = parser.parse_args()
    if args.association_source_categories:
        print(json.dumps(association_source_categories(*args.association_source_categories),
                         sort_keys=True))
        return
    if args.firmware_state:
        difference = firmware_state_difference(*args.firmware_state)
        print(f"firmware_state first_difference={difference}")
        raise SystemExit(difference != "none")
    if args.command_sequence:
        left = command_sequence(args.command_sequence[0])
        right = command_sequence(args.command_sequence[1])
        differences = command_sequence_differences(left, right)
        print(f"command_sequence count={len(left)}/{len(right)}")
        for difference in differences:
            print(f"command_sequence difference={difference}")
        raise SystemExit(bool(differences))
    if not args.manifest:
        parser.error("manifest, --firmware-state, or --command-sequence is required")
    with open(args.manifest, encoding="utf-8") as source:
        manifest = json.load(source)
    if set(manifest) != {"commands", "records"}:
        parser.error(
            "manifest must contain exactly 'commands' and 'records'; "
            "the legacy record-only format could hide missing commands"
        )
    commands = manifest["commands"]
    if set(commands) != {"linux", "userspace"}:
        parser.error("manifest commands must contain exactly 'linux' and 'userspace'")
    left_commands = [
        (int(record["cmd"], 0), int(record["payload_len"]), int(record["wait"]))
        for record in commands["linux"]
    ]
    right_commands = [
        (int(record["cmd"], 0), int(record["payload_len"]), int(record["wait"]))
        for record in commands["userspace"]
    ]
    command_differences = command_sequence_differences(left_commands, right_commands)

    failed = bool(command_differences)
    print(f"command_sequence count={len(left_commands)}/{len(right_commands)}")
    for difference in command_differences:
        print(f"command_sequence difference={difference}")
    records = manifest["records"]
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
