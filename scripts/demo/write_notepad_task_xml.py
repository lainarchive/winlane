#!/usr/bin/env python3
"""Write the scheduled-task XML used to launch Notepad in the interactive Windows session."""

import sys
from pathlib import Path

TASK_XML = r"""<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>winlane demo Notepad launcher</Description></RegistrationInfo>
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
    <Exec><Command>C:\Windows\System32\notepad.exe</Command><Arguments></Arguments></Exec>
  </Actions>
</Task>
"""


def main() -> None:
    if len(sys.argv) != 2:
        print("usage: write_notepad_task_xml.py OUTPUT", file=sys.stderr)
        raise SystemExit(2)
    output = Path(sys.argv[1])
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(TASK_XML, encoding="utf-16")


if __name__ == "__main__":
    main()
