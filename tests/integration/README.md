# Integration tests

The integration ladder is:

1. contract tests
2. workspace build
3. Linux VM boot and screenshot
4. Windows Setup boot
5. unattended Windows desktop
6. checkpoint and restore
7. UIA tree plus one semantic action
8. evidence export for the full run

A stage is not done because code exists. It is done when the runner for that stage passes repeatedly.
