Name:           nyi
Version:        1.0.0
Release:        1.%(date +%%Y%%m%%d)%{?dist}
Summary:        Animated 3D fetch

License:        ISC
URL:            https://github.com/elwrcl/nyi
Source0:        %{url}/archive/refs/heads/main.tar.gz#/nyi-%{version}.tar.gz

BuildRequires:  cargo
BuildRequires:  rust >= 1.74
BuildRequires:  make

Recommends:     fastfetch
Recommends:     pciutils

%description
donut.c inspired fetch tool with shading.
System info is gathered natively from /proc, /sys, and GTK
config — no external runtime dependencies required.

%prep
%autosetup -n nyi-main

%build
%make_build

%install
%make_install DESTDIR=%{buildroot} PREFIX=%{_prefix}

%files
%doc README.md
%license LICENSE
%{_bindir}/nyi

%changelog
* Sat Aug 15 2026 elwrcl <elwerici@icloud.com> - 1.0.0-1
- Initial package.
