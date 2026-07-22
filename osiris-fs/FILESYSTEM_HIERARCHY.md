# Osiris OS - Filesystem Hierarchy Standard
# Based on FHS 3.0 with Osiris-specific extensions

# Root directories
/osiris           # Osiris root (read-only, from .osr packages)
/osiris/bin       # Symlinks to active package binaries
/osiris/lib       # Shared libraries (musl + harvested glibc compat)
/osiris/etc       # System configuration (mutable, bind-mounted from /etc)
/osiris/var       # State data (logs, entropy seed, package DB)
/osiris/run       # Runtime data (tmpfs, sockets, PIDs)
/osiris/home      # User data (separate partition)
/osiris/opt       # Optional/large packages

# Standard symlinks for compatibility
/bin    -> /osiris/bin
/sbin   -> /osiris/bin
/lib    -> /osiris/lib
/lib64  -> /osiris/lib
/usr    -> /osiris

# Osiris-specific mountpoints (created by Kha at boot)
/proc   # procfs
/sys    # sysfs
/dev    # devtmpfs
/run    # tmpfs (bind-mounted to /osiris/run)