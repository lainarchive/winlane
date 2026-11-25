#!/usr/bin/env bash
set -Eeuo pipefail

if [[ $# -ne 4 ]]; then
  echo "usage: $0 GOLDEN_IMAGE REPO_WINLANE_BINARY FINAL_OUTPUT_DIR MACHINE_NAME" >&2
  exit 2
fi

golden_image=$1
source_binary=$2
final_output=$3
machine_name=$4
staging=/tmp/winlane-demo
runtime=/tmp/winlane-demo-runtime
control_socket=/tmp/winlane-demo-control.sock
evidence=/tmp/winlane-demo-evidence
output=/tmp/winlane-demo-output

[[ -f $golden_image ]] || { echo "golden image missing: $golden_image" >&2; exit 2; }
[[ -x $source_binary ]] || { echo "winlane binary missing/not executable: $source_binary" >&2; exit 2; }
[[ $machine_name =~ ^[A-Za-z0-9_-]{1,48}$ ]] || { echo "invalid machine name" >&2; exit 2; }

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
rm -rf -- "$staging" "$runtime" "$evidence" "$output"
rm -f -- "$control_socket"
mkdir -p -- "$staging/bin" "$staging/scripts/demo" "$final_output"
cp -- "$source_binary" "$staging/bin/winlane"
cp -- "$script_dir"/*.sh "$script_dir"/*.py "$script_dir"/*.ps1 "$staging/scripts/demo/"
chmod +x "$staging/bin/winlane" "$staging/scripts/demo"/*.sh
ln -s -- "$(readlink -f "$golden_image")" "$staging/golden.qcow2"

"$staging/scripts/demo/record-x11.sh" \
  --golden-image "$staging/golden.qcow2" \
  --runtime-root "$runtime" \
  --control-socket "$control_socket" \
  --evidence-dir "$evidence" \
  --output-dir "$output" \
  --machine-name "$machine_name" \
  --winlane-bin "$staging/bin/winlane" \
  --kitty-bin /tmp/kitty-modern/bin/kitty \
  --driver-script "$staging/scripts/demo/drive-demo.sh"

cp -a -- "$output/." "$final_output/"
printf 'copied neutral-path output from %s to %s\n' "$output" "$final_output"
