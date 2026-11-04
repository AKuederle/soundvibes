#!/usr/bin/env bash
set -euo pipefail
package_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../contrib/plasmoid" && pwd)"
if kpackagetool6 --type Plasma/Applet --show org.soundvibes.dictation >/dev/null 2>&1; then
    kpackagetool6 --type Plasma/Applet --upgrade "$package_root"
else
    kpackagetool6 --type Plasma/Applet --install "$package_root"
fi
