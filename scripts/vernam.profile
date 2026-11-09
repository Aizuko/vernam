# Firejail profile for Vernam, the Signal client. Wayland only.
#   firejail --profile=scripts/vernam.profile target/linux/release/vernam
# A profile given with --profile replaces firejail's default one, so
# everything this file does not restrict stays reachable: /usr and /etc
# for reading, the network, and /run/user/<uid>, where the Wayland
# compositor socket, the session bus that carries notifications, and the
# pipewire sockets for the microphone and sound live. The paths assume
# the standard XDG layout; with XDG_DATA_HOME pointed somewhere else the
# mkdir and whitelist lines below have to follow it.

# Sandbox shape: capabilities dropped, no new privileges or setuid, no
# /dev/input (Wayland input goes through the compositor anyway), only
# unix, internet and netlink sockets (netlink for interface enumeration,
# which call setup needs), seccomp, no new user namespaces, a private
# /dev, which keeps dri and snd, and a private /tmp.
caps.drop all
noinput
nonewprivs
noroot
protocol unix,inet,inet6,netlink
seccomp
restrict-namespaces
private-tmp
private-dev
# No netfilter: its default rules fence the UDP ports calls use, and
# calls are the one thing this client does besides text.

# The home directory inside the jail reads as empty except for the lines
# below: what Vernam owns (data, single-instance socket, the logs under
# it), the desktop files and icons it writes, the sgnl:// handler list,
# and the caches the graphics stack keeps there. A whitelist entry for a
# path the host does not have yet is dropped silently, and the app would
# then create that path inside the sandbox where it dies with the
# session, so mkdir makes each one real first (existing ones stay).
mkdir ${HOME}/.local
mkdir ${HOME}/.local/share
mkdir ${HOME}/.config
mkdir ${HOME}/.cache
mkdir ${HOME}/.local/share/io.github.knpwrs.Vernam
mkdir ${HOME}/.local/share/applications
mkdir ${HOME}/.local/share/icons
mkdir ${HOME}/.config/autostart
mkdir ${HOME}/.cache/fontconfig
mkdir ${HOME}/.cache/mesa_shader_cache
mkdir ${HOME}/.cache/mesa_shader_cache_db
whitelist ${HOME}/.local/share/io.github.knpwrs.Vernam
whitelist ${HOME}/.local/share/applications
whitelist ${HOME}/.local/share/icons
whitelist ${HOME}/.config/autostart
whitelist ${HOME}/.config/mimeapps.list
whitelist ${HOME}/.cache/fontconfig
whitelist ${HOME}/.cache/mesa_shader_cache
whitelist ${HOME}/.cache/mesa_shader_cache_db
