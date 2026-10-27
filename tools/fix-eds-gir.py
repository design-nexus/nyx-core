"""Copy an installed GIR file so gir can read it: drop functions and methods marked
introspectable="0" and give untyped <type> elements a stand-in.

Those entries (private helpers such as camel_search_build_match_regex) carry
types gir cannot parse, and nothing in Nyx calls them.
"""
import re
import subprocess
import sys
import xml.etree.ElementTree as ET

CORE = "http://www.gtk.org/introspection/core/1.0"
ET.register_namespace("", CORE)
ET.register_namespace("c", "http://www.gtk.org/introspection/c/1.0")
ET.register_namespace("glib", "http://www.gtk.org/introspection/glib/1.0")
DROP = {f"{{{CORE}}}{tag}" for tag in ("function", "method", "constructor", "virtual-method")}

src, dst = sys.argv[1], sys.argv[2]
tree = ET.parse(src)
root = tree.getroot()
count = 0
for parent in tree.iter():
    for child in list(parent):
        if child.tag in DROP and child.get("introspectable") == "0":
            parent.remove(child)
            count += 1
# Untyped <type> elements (private C types such as vcardvalue_kind in callbacks
# nothing calls) get an ABI-equivalent stand-in so gir can parse the file.
CTYPE = "{http://www.gtk.org/introspection/c/1.0}type"
for node in tree.iter(f"{{{CORE}}}type"):
    if node.get("name") is None:
        node.set("name", "gpointer" if (node.get(CTYPE) or "").endswith("*") else "gint")
        count += 1
# A few C types are spelled through their struct tag ("struct _ESource *");
# use the typedef name gir generates.
TAG = re.compile(r"^(const )?(struct )?_([A-Z]\w*)")
for node in tree.iter():
    ctype = node.get(CTYPE)
    if ctype and TAG.match(ctype) and node.tag != f"{{{CORE}}}record":
        node.set(CTYPE, TAG.sub(lambda m: (m.group(1) or "") + m.group(3), ctype))
        count += 1
# Upstream GIR mistakes found by the generated ABI tests (EDS 3.62, libical 4.0):
# constant values that differ from the C headers, a build define exported as a
# constant, and get_type functions the shared library does not export.
CONSTANTS = {
    "NETWORK_MONITOR_ALWAYS_ONLINE_NAME": "always-online",
    "SOURCE_PARAM_SETTING": "256",
    "WEBDAV_COLLATION_ASCII_CASEMAP": "i;ascii-casemap",
    "WEBDAV_COLLATION_ASCII_NUMERIC": "i;ascii-numeric",
    "WEBDAV_COLLATION_OCTET": "i;octet",
    "WEBDAV_COLLATION_UNICODE_CASEMAP": "i;unicode-casemap",
}
DROP_CONSTANTS = {"LIBICAL_GLIB_UNSTABLE_API"}
namespace = root.find(f"{{{CORE}}}namespace")
for const in list(namespace.findall(f"{{{CORE}}}constant")):
    name = const.get("name")
    if name in DROP_CONSTANTS:
        namespace.remove(const)
        count += 1
    elif name in CONSTANTS and namespace.get("name") == "EDataServer":
        const.set("value", CONSTANTS[name])
        count += 1

# libical-glib's I_CAL_VCARD_TYPE_PARAMETER enum member is shadowed in C by the
# GType macro of the same name, so it cannot be checked or used from C either.
SHADOWED_MEMBERS = {"I_CAL_VCARD_TYPE_PARAMETER"}
for parent in namespace.iter():
    for member in list(parent.findall(f"{{{CORE}}}member")):
        if member.get("{http://www.gtk.org/introspection/c/1.0}identifier") in SHADOWED_MEMBERS:
            parent.remove(member)
            count += 1

GET_TYPE = "{http://www.gtk.org/introspection/glib/1.0}get-type"
libraries = [lib for lib in namespace.get("shared-library", "").split(",") if lib]
if libraries:
    exported = set()
    for library in libraries:
        listing = subprocess.run(["nm", "-D", "--defined-only", f"/usr/lib/{library}"],
                                 capture_output=True, text=True, check=True).stdout
        exported |= {word.split("@")[0] for word in listing.split()}
    for node in namespace.iter():
        symbol = node.get(GET_TYPE)
        if symbol and symbol != "intern" and symbol not in exported:
            del node.attrib[GET_TYPE]
            count += 1
tree.write(dst, encoding="utf-8", xml_declaration=True)
print(f"{src}: {count} fixes", file=sys.stderr)
