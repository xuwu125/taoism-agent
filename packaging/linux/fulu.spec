# RPM 打包 spec（T26 / v0.4.0 跨平台发布矩阵）
#
# 由 release.yml 的 "Installer (Linux RPM)" 步骤调用，架构/版本/stage 目录通过
# rpmbuild --define 注入：
#   rpmbuild -bb \
#     --define "_topdir <stage/rpmbuild>" \
#     --define "fulu_version 0.4.0" \
#     --define "fulu_arch x86_64|aarch64" \
#     --define "fulu_bindir <abs path to stage dir>" \
#     packaging/linux/fulu.spec
#
# 约定与 DEB 包一致：明文二进制 + 许可全文 + 模型清单；不写系统级配置，
# 不内置权重（离线 bundle 是独立的 opt-in 流程）。

Name:           fulu
Version:        %{fulu_version}
Release:        1%{?dist}
Summary:        符箓台 fulu - 本地优先的多智能体符箓台

License:        MIT AND OFL-1.1 AND Ubuntu-font-1.0 AND CDLA-Permissive-2.0
URL:            https://github.com/xuwu125/taoism-agent
BuildArch:      %{fulu_arch}

# 运行时依赖：与 DEB control 的 Depends 对齐，X11/Wayland/GL/中文字体回退所需。
Requires:       glibc
Requires:       libxkbcommon
Requires:       libX11
Requires:       libXcursor
Requires:       libXrandr
Requires:       libXi
Requires:       mesa-libGL

%description
本地 LFM 模型驱动的多智能体系统：符（指令）/ 箓（授权名册）/ 篆（编码书写）
三层，配法坛运行时与缴符审计。不静默联网。本包含 fulu（命令行）与
fulu-ui（符箓台 GUI）两个可执行文件。

%install
mkdir -p %{buildroot}/usr/bin
mkdir -p %{buildroot}/usr/share/doc/fulu/licenses
mkdir -p %{buildroot}/usr/share/applications

install -m 0755 %{fulu_bindir}/fulu     %{buildroot}/usr/bin/fulu
install -m 0755 %{fulu_bindir}/fulu-ui  %{buildroot}/usr/bin/fulu-ui

install -m 0644 %{fulu_bindir}/LICENSE               %{buildroot}/usr/share/doc/fulu/LICENSE
install -m 0644 %{fulu_bindir}/THIRD_PARTY_NOTICES.md %{buildroot}/usr/share/doc/fulu/THIRD_PARTY_NOTICES.md
install -m 0644 %{fulu_bindir}/manifest.json          %{buildroot}/usr/share/doc/fulu/manifest.json
install -m 0644 %{fulu_bindir}/README.md              %{buildroot}/usr/share/doc/fulu/README.md

# T07：二进制内嵌字体 = 分发字体，许可全文必须随包（两条 cp 覆盖同名文件）。
cp %{fulu_bindir}/licenses/*.txt %{buildroot}/usr/share/doc/fulu/licenses/

%files
/usr/bin/fulu
/usr/bin/fulu-ui
/usr/share/doc/fulu/LICENSE
/usr/share/doc/fulu/THIRD_PARTY_NOTICES.md
/usr/share/doc/fulu/manifest.json
/usr/share/doc/fulu/README.md
/usr/share/doc/fulu/licenses/*.txt

%changelog
* Sat Nov 21 2026 taoism-agent <noreply@example.invalid> - %{fulu_version}-1
- 符箓台跨平台发布：x86_64 / aarch64 RPM 包
