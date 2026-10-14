# Maintainer: toby <https://git.tsndr.io/tsndr/dbased>
pkgname=dbased
pkgver=0.1.0
pkgrel=1
pkgdesc="Clean Libadwaita GUI Database Client for MySQL and PostgreSQL"
arch=('x86_64' 'aarch64')
url="https://git.tsndr.io/tsndr/dbased"
license=('MIT')
depends=(
    'gcc-libs'
    'glibc'
    'gtk4'
    'gtksourceview5'
    'hicolor-icon-theme'
    'libadwaita'
)
makedepends=('cargo')
options=('!lto' '!debug')

# Keep makepkg's work directory inside target/makepkg when run in-tree so
# makepkg's default $srcdir ($startdir/src) never collides with Rust's ./src.
if [[ -z "${BUILDDIR:-}" || "$BUILDDIR" -ef "$startdir" ]]; then
    BUILDDIR="$startdir/target/makepkg"
    mkdir -p "$BUILDDIR"
fi

prepare() {
    cd "$startdir"
    export RUSTUP_TOOLCHAIN=stable
    cargo fetch --locked --target "$(rustc -vV | sed -n 's/host: //p')"
}

build() {
    cd "$startdir"
    export RUSTUP_TOOLCHAIN=stable
    export CARGO_TARGET_DIR=target
    cargo build --frozen --release
}

package() {
    cd "$startdir"
    make PREFIX=/usr DESTDIR="$pkgdir" install-files
}
