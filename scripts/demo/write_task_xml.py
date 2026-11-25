#!/usr/bin/env python3
"""Write scheduled-task XML for a command in the interactive Windows session."""

import argparse
from pathlib import Path
from xml.sax.saxutils import escape

TASK_XML = """<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>winlane demo interactive task</Description></RegistrationInfo>
  <Triggers><TimeTrigger><Enabled>false</Enabled><StartBoundary>2024-01-01T00:00:00</StartBoundary></TimeTrigger></Triggers>
  <Principals>
    <Principal id="Author"><UserId>INTERACTIVE</UserId><LogonType>InteractiveToken</LogonType><RunLevel>HighestAvailable</RunLevel></Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <ExecutionTimeLimit>PT5M</ExecutionTimeLimit>
    <Priority>4</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec><Command>{command}</Command><Arguments>{arguments}</Arguments></Exec>
  </Actions>
</Task>
"""


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("output", type=Path)
    parser.add_argument("--command", required=True)
    parser.add_argument("--arguments", default="")
    args = parser.parse_args()
    if not args.command.strip():
        parser.error("--command must be nonempty")
    if any(ord(character) < 32 for character in args.command + args.arguments):
        parser.error("command and arguments must not contain control characters")
    xml = TASK_XML.format(command=escape(args.command), arguments=escape(args.arguments))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(xml, encoding="utf-16")


if __name__ == "__main__":
    main()
