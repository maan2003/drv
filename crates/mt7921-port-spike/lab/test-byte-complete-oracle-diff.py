#!/usr/bin/env python3

import importlib.util
import pathlib
import unittest


HERE = pathlib.Path(__file__).parent
SPEC = importlib.util.spec_from_file_location(
    "byte_complete_oracle_diff", HERE / "byte-complete-oracle-diff.py"
)
ORACLE_DIFF = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ORACLE_DIFF)


class CommandSequenceTest(unittest.TestCase):
    def sequence(self, name):
        return ORACLE_DIFF.command_sequence(HERE / "fixtures" / name)

    def test_identical_sequence_has_no_difference(self):
        linux = self.sequence("command-sequence-linux.log")
        self.assertEqual(ORACLE_DIFF.command_sequence_differences(linux, linux), [])

    def test_missing_commands_are_reported(self):
        differences = ORACLE_DIFF.command_sequence_differences(
            self.sequence("command-sequence-linux.log"),
            self.sequence("command-sequence-userspace-missing.log"),
        )
        report = "\n".join(differences)
        self.assertIn("cmd=0x400a1,payload_len=8,wait=0", report)
        self.assertIn("cmd=0x20027,payload_len=16,wait=0", report)

    def test_extra_command_is_reported(self):
        differences = ORACLE_DIFF.command_sequence_differences(
            self.sequence("command-sequence-linux.log"),
            self.sequence("command-sequence-userspace-extra.log"),
        )
        self.assertIn(
            "cmd=0x400ff,payload_len=4,wait=0", "\n".join(differences)
        )


if __name__ == "__main__":
    unittest.main()
