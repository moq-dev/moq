#!/usr/bin/env bash
# Copy an offline ext4 runner into a new XFS image, retaining its original disk.
set -euo pipefail
export LIBGUESTFS_BACKEND=direct
: "${runner_image:?}" "${runner_capacity:?}"

if [[ $# != 2 ]]; then
    echo "usage: runner-migrate SOURCE.qcow2 DESTINATION.raw" >&2
    exit 2
fi
source_disk=$(realpath -e -- "$1")
destination=$(realpath -m -- "$2")
if [[ -e "$destination" ]]; then
    echo "Destination already exists: $destination" >&2
    exit 1
fi
if fuser "$source_disk" >/dev/null 2>&1; then
    echo "Source disk is in use; stop its VM before migrating." >&2
    exit 1
fi

temporary=$(mktemp --tmpdir="$(dirname "$destination")" .runner-migrate.XXXXXXXX)
trap 'rm -f -- "$temporary"' EXIT
cp --reflink=auto --sparse=always "$runner_image" "$temporary"
chmod 0600 "$temporary"
chown --reference="$source_disk" "$temporary"
image_size=$(stat -c %s "$temporary")
qemu-img resize -f raw "$temporary" "$runner_capacity"

# Let GPT tooling preserve its metadata reservation rather than guessing the
# last usable sector from the disk size.
grow_partition=
if (($(stat -c %s "$temporary") > image_size)); then
    sgdisk --move-second-header "$temporary"
    last_sector=$(sgdisk --end-of-largest "$temporary")
    grow_partition="part-resize /dev/sda 2 $last_sector"
fi
guestfish --rw --format=raw -a "$temporary" <<EOF
run
$grow_partition
mount /dev/sda2 /
xfs-growfs /
EOF

# guestfish keeps guest filesystems inside an appliance rather than mounting
# them in the host kernel. The new image's bootstrap manifest must survive.
guestfish --ro --format=qcow2 -a "$source_disk" -m /dev/sda1 \
    tar-out / - numericowner:true xattrs:true acls:true excludes:./nix-path-registration |
    guestfish --rw --format=raw -a "$temporary" -m /dev/sda2 \
        tar-in - / xattrs:true acls:true

mv -n -- "$temporary" "$destination"
test ! -e "$temporary"
echo "Migrated to $destination; original disk retained at $source_disk."
