# Fedora COPR build of pova, from source with vendored crates (offline build).
# The source tarballs are made by packaging/copr/make-srpm.sh (see .copr/Makefile).

# The release profile strips the binary, so there is no debuginfo to package.
%global debug_package %{nil}

Name:           pova
Version:        1.3.0
Release:        1%{?dist}
Summary:        YouTube Music in the terminal, with a cava visualizer and btop themes
License:        MIT
URL:            https://github.com/chrollo-03/pova
Source0:        pova-%{version}.tar.gz
Source1:        pova-%{version}-vendor.tar.gz

ExclusiveArch:  x86_64 aarch64
BuildRequires:  rust >= 1.90
BuildRequires:  cargo

Requires:       mpv
Requires:       yt-dlp
Requires:       pipewire-utils
Recommends:     nodejs
Recommends:     python3
Recommends:     mpv-mpris
Recommends:     btop
Recommends:     libnotify

%description
pova plays YouTube Music in the terminal with a btop-style layout, a
visualizer ported from cava that listens only to pova's own audio, synced
lyrics, album art, a queue, likes and every btop theme.

%prep
%autosetup -n pova-%{version}
tar --no-same-owner -xzf %{SOURCE1}
mkdir -p .cargo
cat > .cargo/config.toml <<'CFG'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
CFG

%build
cargo build --release --locked --offline

%install
install -Dm755 target/release/pova %{buildroot}%{_bindir}/pova
install -Dm755 scripts/import-cookie.sh %{buildroot}%{_bindir}/pova-import-cookie
install -Dm644 packaging/appimage/pova.desktop %{buildroot}%{_datadir}/applications/pova.desktop
install -Dm644 packaging/appimage/pova.png %{buildroot}%{_datadir}/icons/hicolor/512x512/apps/pova.png

%files
%license LICENSE THIRD_PARTY_NOTICES.md
%doc README.md CHANGELOG.md
%{_bindir}/pova
%{_bindir}/pova-import-cookie
%{_datadir}/applications/pova.desktop
%{_datadir}/icons/hicolor/512x512/apps/pova.png

%changelog
* Wed Nov 25 2026 chrollo-03 <chrollo-03@users.noreply.github.com> - 1.3.0-1
- pova 1.3.0 (browser profile sign-in, Brand Account switcher, smoother UI)

* Wed Nov 25 2026 P0r0ro <250746769+P0r0ro@users.noreply.github.com> - 1.2.2-1
- pova 1.2.2

* Wed Nov 25 2026 P0r0ro <250746769+P0r0ro@users.noreply.github.com> - 1.2.1-1
- pova 1.2.1

* Mon Nov 23 2026 P0r0ro <250746769+P0r0ro@users.noreply.github.com> - 1.2.0-1
- pova 1.2.0

* Mon Nov 23 2026 P0r0ro <250746769+P0r0ro@users.noreply.github.com> - 1.1.2-1
- pova 1.1.2

* Mon Nov 23 2026 P0r0ro <250746769+P0r0ro@users.noreply.github.com> - 1.1.1-1
- pova 1.1.1

* Mon Nov 23 2026 P0r0ro <250746769+P0r0ro@users.noreply.github.com> - 1.1.0-1
- pova 1.1.0

* Sun Nov 22 2026 P0r0ro <250746769+P0r0ro@users.noreply.github.com> - 1.0.1-1
- First COPR build
