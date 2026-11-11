#!/usr/bin/env bash
# Builds the signed apt and rpm repositories served at <mirror>/packages/{apt,rpm}.
#
# Usage:
#   packaging/build-repo.sh <packages-dir> <output-dir> <gpg-key-id>
#
#   <packages-dir>  directory containing the .deb and .rpm files to publish
#   <output-dir>    where to write the repository tree (e.g. www/dist/packages); must not exist
#   <gpg-key-id>    signing key, already in the gpg keyring (fingerprint or uid)
#
# Environment:
#   PACKAGES_GPG_PASSPHRASE    passphrase for the signing key (optional)
#   HASHIVERSE_PACKAGE_MIRRORS space-separated base URLs of the /packages directory on every
#                              mirror, in failover order (default: www.hashiverse.{com,eu,ch})
#
# Needs: gpg, apt-ftparchive (apt-utils), rpmsign (rpm), createrepo_c, gzip.
#
# Output:
#   <output-dir>/hashiverse-packages.asc         public signing key
#   <output-dir>/apt/dists/stable/...            apt metadata (InRelease, Release, Release.gpg, Packages)
#   <output-dir>/apt/pool/main/*.deb
#   <output-dir>/apt/hashiverse.sources          client config -> /etc/apt/sources.list.d/
#   <output-dir>/apt/hashiverse-mirrors.txt      client mirror list -> /etc/apt/
#   <output-dir>/rpm/*.rpm, rpm/repodata/        signed rpms and metadata (repomd.xml.asc)
#   <output-dir>/rpm/hashiverse.repo             client config -> /etc/yum.repos.d/
set -euo pipefail

if [ "$#" -ne 3 ]; then
    sed -n "2,9p" "$0" >&2
    exit 2
fi

packages_dir="$(cd "$1" && pwd)"
output_dir="$2"
gpg_key_id="$3"
mirrors="${HASHIVERSE_PACKAGE_MIRRORS:-https://www.hashiverse.com/packages https://www.hashiverse.eu/packages https://www.hashiverse.ch/packages}"
architectures="amd64 arm64"

if [ -e "$output_dir" ]; then
    echo "output dir $output_dir already exists" >&2
    exit 1
fi
mkdir -p "$output_dir"
output_dir="$(cd "$output_dir" && pwd)"

shopt -s nullglob
deb_files=("$packages_dir"/*.deb)
rpm_files=("$packages_dir"/*.rpm)
if [ "${#deb_files[@]}" -eq 0 ] || [ "${#rpm_files[@]}" -eq 0 ]; then
    echo "expected both .deb and .rpm files in $packages_dir" >&2
    exit 1
fi

# --- gpg helpers -----------------------------------------------------------------------

passphrase_file=""
cleanup() {
    if [ -n "$passphrase_file" ]; then
        rm -f "$passphrase_file"
    fi
}
trap cleanup EXIT

gpg_sign_args=(--batch --yes --local-user "$gpg_key_id")
if [ -n "${PACKAGES_GPG_PASSPHRASE:-}" ]; then
    passphrase_file="$(mktemp)"
    chmod 600 "$passphrase_file"
    printf "%s" "$PACKAGES_GPG_PASSPHRASE" > "$passphrase_file"
    gpg_sign_args+=(--pinentry-mode loopback --passphrase-file "$passphrase_file")
fi

gpg --armor --export "$gpg_key_id" > "$output_dir/hashiverse-packages.asc"
if [ ! -s "$output_dir/hashiverse-packages.asc" ]; then
    echo "gpg key $gpg_key_id not found in the keyring" >&2
    exit 1
fi

# --- apt -------------------------------------------------------------------------------

apt_dir="$output_dir/apt"
mkdir -p "$apt_dir/pool/main"
cp "${deb_files[@]}" "$apt_dir/pool/main/"

(
    cd "$apt_dir"
    for architecture in $architectures; do
        binary_dir="dists/stable/main/binary-$architecture"
        mkdir -p "$binary_dir"
        apt-ftparchive --arch "$architecture" packages pool/main > "$binary_dir/Packages"
        gzip -9 --keep --no-name "$binary_dir/Packages"
    done

    apt-ftparchive \
        -o APT::FTPArchive::Release::Origin=Hashiverse \
        -o APT::FTPArchive::Release::Label=Hashiverse \
        -o APT::FTPArchive::Release::Suite=stable \
        -o APT::FTPArchive::Release::Codename=stable \
        -o APT::FTPArchive::Release::Architectures="$architectures" \
        -o APT::FTPArchive::Release::Components=main \
        -o APT::FTPArchive::Release::Description="Hashiverse packages" \
        release dists/stable > dists/stable/Release.tmp
    mv dists/stable/Release.tmp dists/stable/Release

    gpg "${gpg_sign_args[@]}" --clearsign --output dists/stable/InRelease dists/stable/Release
    gpg "${gpg_sign_args[@]}" --armor --detach-sign --output dists/stable/Release.gpg dists/stable/Release
)

# Client config. apt's mirror+file method tries the listed mirrors in order.
for mirror in $mirrors; do
    echo "$mirror/apt"
done > "$apt_dir/hashiverse-mirrors.txt"

cat > "$apt_dir/hashiverse.sources" <<'EOF'
Types: deb
URIs: mirror+file:/etc/apt/hashiverse-mirrors.txt
Suites: stable
Components: main
Signed-By: /usr/share/keyrings/hashiverse-packages.asc
EOF

# --- rpm -------------------------------------------------------------------------------

rpm_dir="$output_dir/rpm"
mkdir -p "$rpm_dir"
cp "${rpm_files[@]}" "$rpm_dir/"

rpmsign_gpg_extra_args="--batch --pinentry-mode loopback"
if [ -n "$passphrase_file" ]; then
    rpmsign_gpg_extra_args="$rpmsign_gpg_extra_args --passphrase-file $passphrase_file"
fi
rpmsign --addsign \
    --define "_gpg_name $gpg_key_id" \
    --define "_gpg_sign_cmd_extra_args $rpmsign_gpg_extra_args" \
    "$rpm_dir"/*.rpm

createrepo_c --quiet "$rpm_dir"
gpg "${gpg_sign_args[@]}" --armor --detach-sign --output "$rpm_dir/repodata/repomd.xml.asc" "$rpm_dir/repodata/repomd.xml"

# Client config. dnf fails over between the baseurls in order.
{
    echo "[hashiverse]"
    echo "name=Hashiverse"
    first=true
    for mirror in $mirrors; do
        if [ "$first" = true ]; then
            echo "baseurl=$mirror/rpm"
            first=false
        else
            echo "        $mirror/rpm"
        fi
    done
    echo "enabled=1"
    echo "gpgcheck=1"
    echo "repo_gpgcheck=1"
    first=true
    for mirror in $mirrors; do
        if [ "$first" = true ]; then
            echo "gpgkey=$mirror/hashiverse-packages.asc"
            first=false
        else
            echo "       $mirror/hashiverse-packages.asc"
        fi
    done
} > "$rpm_dir/hashiverse.repo"

echo "built repository in $output_dir"
