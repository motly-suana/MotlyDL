"""Writes package-manager files for a published MotlyDL release.

    python packaging/make-manifests.py v0.4.2

winget: packaging/out/winget/manifests/m/motly-suana/MotlyDL/<version>/  (submit with `wingetcreate submit`
or a pull request to microsoft/winget-pkgs)
Homebrew: packaging/out/homebrew/Casks/motlydl.rb

The file hashes come from GitHub, which records a SHA-256 for every release file.
"""
import json, subprocess, sys
from pathlib import Path

REPO = "motly-suana/MotlyDL"
tag = sys.argv[1] if len(sys.argv) > 1 else "latest"
args = ["gh", "api", f"repos/{REPO}/releases/" + ("latest" if tag == "latest" else f"tags/{tag}")]
rel = json.loads(subprocess.run(args, capture_output=True, text=True, encoding="utf-8", check=True).stdout)
version = rel["tag_name"].lstrip("v")
date = rel["published_at"][:10]
assets = {a["name"]: a for a in rel["assets"]}

def find(suffix):
    for name, a in assets.items():
        if name.endswith(suffix):
            return a
    return None

def sha(a):
    d = a.get("digest") or ""
    if not d.startswith("sha256:"):
        sys.exit(f"{a['name']} has no SHA-256 recorded by GitHub")
    return d[7:]

out = Path(__file__).parent / "out"

# ---- winget ----
installers = []
for arch, suffixes in (("x64", ("_x64-setup.exe",)), ("arm64", ("_arm64_installer.exe", "_arm64-setup.exe"))):
    a = next((x for x in (find(sfx) for sfx in suffixes) if x), None)
    if a:
        installers.append(f"""- Architecture: {arch}
  InstallerUrl: {a['browser_download_url']}
  InstallerSha256: {sha(a).upper()}""")
if not installers:
    sys.exit("no Windows installer in this release")
w = out / "winget" / "manifests" / "m" / "motly-suana" / "MotlyDL" / version
w.mkdir(parents=True, exist_ok=True)
ident = f"PackageIdentifier: motly-suana.MotlyDL\nPackageVersion: {version}\n"
(w / "motly-suana.MotlyDL.yaml").write_text(ident + "DefaultLocale: en-US\nManifestType: version\nManifestVersion: 1.6.0\n", encoding="utf-8")
(w / "motly-suana.MotlyDL.installer.yaml").write_text(
    ident + f"""InstallerType: nullsoft
Scope: user
InstallModes:
- interactive
- silent
- silentWithProgress
InstallerSwitches:
  Silent: /S
  SilentWithProgress: /S
UpgradeBehavior: install
ReleaseDate: {date}
Installers:
""" + "\n".join(installers) + "\nManifestType: installer\nManifestVersion: 1.6.0\n", encoding="utf-8")
(w / "motly-suana.MotlyDL.locale.en-US.yaml").write_text(
    ident + """PackageLocale: en-US
Publisher: MotlyDL contributors
PublisherUrl: https://github.com/motly-suana
PackageName: MotlyDL
PackageUrl: https://github.com/motly-suana/MotlyDL
License: MIT
LicenseUrl: https://github.com/motly-suana/MotlyDL/blob/main/LICENSE
ShortDescription: Fast, resumable download manager with a browser extension
Description: MotlyDL splits each download across many connections, resumes after crashes or network drops, catches downloads from your browser, and downloads video streams.
Moniker: motlydl
Tags:
- download-manager
- downloader
- video-downloader
- torrent
ReleaseNotesUrl: """ + rel["html_url"] + """
ManifestType: defaultLocale
ManifestVersion: 1.6.0
""", encoding="utf-8")
print(f"winget manifests: {w}")

# ---- Homebrew cask ----
dmg = find("_universal.dmg")
if dmg:
    c = out / "homebrew" / "Casks"
    c.mkdir(parents=True, exist_ok=True)
    (c / "motlydl.rb").write_text(f'''cask "motlydl" do
  version "{version}"
  sha256 "{sha(dmg)}"

  url "https://github.com/{REPO}/releases/download/v#{{version}}/MotlyDL_#{{version}}_universal.dmg"
  name "MotlyDL"
  desc "Fast, resumable download manager with a browser extension"
  homepage "https://github.com/{REPO}"

  livecheck do
    url :url
    strategy :github_latest
  end

  app "MotlyDL.app"

  zap trash: [
    "~/Library/Application Support/com.motlydl.app",
    "~/Library/Caches/com.motlydl.app",
  ]
end
''', encoding="utf-8")
    print(f"Homebrew cask: {c / 'motlydl.rb'}")
