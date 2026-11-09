# Firejail profile for Vernam, the Signal client. Wayland only.
#   firejail --profile=scripts/vernam.profile target/linux/release/vernam
# A profile given with --profile replaces firejail's default one, so
# everything this file does not restrict stays reachable: /usr and /etc
# for reading and the network. The paths assume the standard XDG layout;
# with XDG_DATA_HOME pointed somewhere else the mkdir and whitelist
# lines below have to follow it. The executable also has to be visible
# inside the jail: when the checkout lives under home, run a copy from
# outside it (say /usr/local/bin) or whitelist the checkout directory.

# Sandbox shape: capabilities dropped, no new privileges or setuid, no
# /dev/input (Wayland input goes through the compositor anyway), no X11
# (x11 none refuses the launch when an abstract X11 socket is reachable
# in the jail; if your Xwayland still opens one, this line has to go),
# only unix, internet and netlink sockets (netlink for interface
# enumeration, which call setup needs), seccomp, no new user namespaces,
# private IPC, a private /dev, which keeps dri, snd and video capture
# for calls but on proprietary NVIDIA hosts leaves out the nvidia nodes
# (there, drop private-dev to render), and a private /tmp. dvd, tv and
# FIDO devices have no business here.
caps.drop all
ipc-namespace
noinput
nonewprivs
noroot
nodvd
notv
nou2f
protocol unix,inet,inet6,netlink
seccomp
restrict-namespaces
private-tmp
private-dev
x11 none
# No netfilter: its default rules fence the UDP ports calls use, and
# calls are the one thing this client does besides text.

# The buses, filtered: on the session bus notifications (with Plasma's
# reply field) and the portals the file dialogs go through; on the
# system bus logind's wake signal, which reconnects the session after
# the machine wakes. Everything else stays out. Both filters need the
# xdg-dbus-proxy binary; without it firejail warns and leaves the bus
# unfiltered.
dbus-system filter
dbus-system.talk org.freedesktop.login1
dbus-user filter
dbus-user.talk org.freedesktop.Notifications
dbus-user.talk org.freedesktop.portal.Desktop

# The user runtime directory: only the compositor, bus and audio
# sockets, as firejail's own whitelist-runuser-common.inc whitelists
# them, so the ssh agent and every other session service in there stays
# invisible. The globs cover the compositor's socket numbering.
whitelist ${RUNUSER}/bus
whitelist ${RUNUSER}/pulse/native
whitelist ${RUNUSER}/pipewire-?
whitelist ${RUNUSER}/wayland-?

# The home directory inside the jail reads as empty except for the lines
# below: what Vernam owns (data, the hand-off socket for sgnl:// links,
# the logs under it), the font and shader caches the graphics stack
# keeps there, and Downloads for the file dialogs (attach and save-as).
# Nothing of the desktop registration is whitelisted: autostart entries
# and Exec= lines are host-execution mechanisms that outlive the
# sandbox, so the host copies are written by unjailed launches only
# (register_desktop runs on every launch; from inside the jail its
# writes vanish with it, and the open-at-login toggle does nothing).
# A whitelist entry for a path the host does not have yet is dropped
# silently, and the app would then create that path inside the sandbox
# where it dies with the session, so mkdir makes each one real first
# (existing ones stay).
mkdir ${HOME}/.local
mkdir ${HOME}/.local/share
mkdir ${HOME}/.cache
mkdir ${HOME}/.local/share/io.github.knpwrs.Vernam
mkdir ${HOME}/.cache/fontconfig
mkdir ${HOME}/.cache/mesa_shader_cache
mkdir ${HOME}/.cache/mesa_shader_cache_db
mkdir ${HOME}/Downloads
whitelist ${HOME}/.local/share/io.github.knpwrs.Vernam
whitelist ${HOME}/.cache/fontconfig
whitelist ${HOME}/.cache/mesa_shader_cache
whitelist ${HOME}/.cache/mesa_shader_cache_db
whitelist ${HOME}/Downloads
