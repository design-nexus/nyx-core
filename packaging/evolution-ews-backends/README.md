# evolution-ews-backends

Microsoft 365, Outlook.com and Exchange support for Evolution Data Server, without the
Evolution mail client. Calendar, Reminders and Contacts read Microsoft 365 calendars, To
Do lists and address books through it. Mail talks to Microsoft Graph itself and doesn't
need it.

It is GNOME's `evolution-ews` built with `-DWITH_EVOLUTION=OFF`: the same backends as
Arch's `evolution-ews` package, without the two Evolution settings modules, so it doesn't
pull in `evolution` (about 85 MB). It provides and conflicts with `evolution-ews`.

## Build and install

```sh
sudo pacman -S --needed base-devel cmake ninja glib2-devel intltool
makepkg -si
```

If Arch's `evolution-ews` is installed, pacman offers to replace it. Evolution itself can
then go: `sudo pacman -Rns evolution`. Restart Evolution Data Server so it loads the new
modules:

```sh
systemctl --user restart evolution-source-registry evolution-calendar-factory evolution-addressbook-factory
```

## Updating

The modules only load into the `evolution-data-server` series they were built for
(`_eds` in the PKGBUILD). When a system upgrade brings a new series, pacman stops the
whole upgrade with a dependency conflict until this package is rebuilt for it (or removed). For each new GNOME release: set `pkgver`,
`_eds` and `_eds_next`, update `sha256sums` from GNOME's `.sha256sum` file, and build.
