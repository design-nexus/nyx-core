//! Turn GetManagedObjects (`a{oa{sa{sv}}}`) into [`Account`] values.

use super::{Account, Service, ServiceState, ACCOUNT_IFACE};

const DISABLED: &[(&str, Service)] = &[
    ("MailDisabled", Service::Mail),
    ("CalendarDisabled", Service::Calendar),
    ("ContactsDisabled", Service::Contacts),
    ("TodoDisabled", Service::Tasks),
    ("FilesDisabled", Service::Files),
    ("ChatDisabled", Service::Chat),
    ("PhotosDisabled", Service::Photos),
    ("TicketingDisabled", Service::Ticketing),
    ("MusicDisabled", Service::Music),
];

pub fn accounts_from_managed_objects(objects: &glib::Variant) -> Vec<Account> {
    let mut out = Vec::new();
    if !objects.is_container() {
        return out;
    }
    for i in 0..objects.n_children() {
        let child = objects.child_value(i);
        if let Some(account) = account_from_entry(&child) {
            out.push(account);
        }
    }
    out.sort_by(|a, b| a.presentation.to_lowercase().cmp(&b.presentation.to_lowercase()).then(a.id.cmp(&b.id)));
    out
}

fn account_from_entry(entry: &glib::Variant) -> Option<Account> {
    if entry.n_children() < 2 {
        return None;
    }
    let path = string_child(&entry.child_value(0))?;
    let ifaces = entry.child_value(1);
    let account_props = iface_props(&ifaces, ACCOUNT_IFACE)?;
    let id = prop_string(&account_props, "Id").unwrap_or_else(|| path.rsplit('/').next().unwrap_or("").to_string());
    let mut services = services_from(&ifaces, &account_props);
    services.sort();
    Some(Account {
        id,
        path,
        provider_type: prop_string(&account_props, "ProviderType").unwrap_or_default(),
        provider_name: prop_string(&account_props, "ProviderName").unwrap_or_default(),
        provider_icon: prop_string(&account_props, "ProviderIcon").unwrap_or_default(),
        identity: prop_string(&account_props, "Identity").unwrap_or_default(),
        presentation: prop_string(&account_props, "PresentationIdentity").unwrap_or_default(),
        attention_needed: prop_bool(&account_props, "AttentionNeeded").unwrap_or(false),
        locked: prop_bool(&account_props, "IsLocked").unwrap_or(false),
        services,
    })
}

fn services_from(ifaces: &glib::Variant, account_props: &glib::Variant) -> Vec<ServiceState> {
    let present: Vec<Service> = iface_names(ifaces).iter().filter_map(|n| Service::from_interface(n)).collect();
    let mut out = Vec::new();
    for &(key, ref service) in DISABLED {
        let Some(disabled) = prop_bool(account_props, key) else { continue };
        let offered = present.iter().any(|s| s == service);
        // Google still publishes FilesDisabled etc. for features it no longer offers.
        // Keep a switch only if the interface is here, or the user already turned it off.
        if !offered && !disabled {
            continue;
        }
        out.push(ServiceState { service: service.clone(), enabled: !disabled, can_toggle: true });
    }
    for service in present {
        if !out.iter().any(|s| s.service == service) {
            out.push(ServiceState { service, enabled: true, can_toggle: false });
        }
    }
    out
}

fn iface_names(ifaces: &glib::Variant) -> Vec<String> {
    let mut names = Vec::new();
    if !ifaces.is_container() {
        return names;
    }
    for i in 0..ifaces.n_children() {
        let entry = ifaces.child_value(i);
        if entry.n_children() >= 1
            && let Some(name) = string_child(&entry.child_value(0))
        {
            names.push(name);
        }
    }
    names
}

pub(crate) fn iface_props(ifaces: &glib::Variant, name: &str) -> Option<glib::Variant> {
    if !ifaces.is_container() {
        return None;
    }
    for i in 0..ifaces.n_children() {
        let entry = ifaces.child_value(i);
        if entry.n_children() >= 2 && string_child(&entry.child_value(0)).as_deref() == Some(name) {
            return Some(entry.child_value(1));
        }
    }
    None
}

pub(crate) fn prop_string(props: &glib::Variant, key: &str) -> Option<String> {
    variant_prop(props, key).and_then(|v| {
        if v.is_container() && v.n_children() == 1 {
            string_child(&v.child_value(0)).or_else(|| v.child_value(0).str().map(str::to_string))
        } else {
            v.str().map(str::to_string)
        }
    })
}

pub(crate) fn prop_bool(props: &glib::Variant, key: &str) -> Option<bool> {
    let v = variant_prop(props, key)?;
    let inner = if v.is_container() && v.n_children() == 1 { v.child_value(0) } else { v };
    inner.get::<bool>()
}

fn variant_prop(props: &glib::Variant, key: &str) -> Option<glib::Variant> {
    if !props.is_container() {
        return None;
    }
    for i in 0..props.n_children() {
        let entry = props.child_value(i);
        if entry.n_children() >= 2 && string_child(&entry.child_value(0)).as_deref() == Some(key) {
            return Some(entry.child_value(1));
        }
    }
    None
}

pub(crate) fn string_child(v: &glib::Variant) -> Option<String> {
    v.str().map(str::to_string).or_else(|| v.get::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use glib::prelude::*;
    use glib::variant::ObjectPath;
    use glib::{Variant, VariantTy};

    fn asv(pairs: &[(&str, Variant)]) -> Variant {
        let dict = glib::VariantDict::new(None);
        for (k, v) in pairs {
            dict.insert_value(k, v);
        }
        dict.to_variant()
    }

    fn iface_dict(pairs: &[(&str, Variant)]) -> Variant {
        let entries: Vec<Variant> = pairs
            .iter()
            .map(|(name, props)| Variant::from_dict_entry(&name.to_variant(), props))
            .collect();
        Variant::array_from_iter_with_type(VariantTy::new("{sa{sv}}").unwrap(), entries)
    }

    #[test]
    fn parses_google_account() {
        let account = asv(&[
            ("Id", "account_1".to_variant()),
            ("ProviderType", "google".to_variant()),
            ("ProviderName", "Google".to_variant()),
            ("ProviderIcon", "goa-account-google".to_variant()),
            ("Identity", "a@example.com".to_variant()),
            ("PresentationIdentity", "a@example.com".to_variant()),
            ("AttentionNeeded", false.to_variant()),
            ("IsLocked", false.to_variant()),
            ("MailDisabled", false.to_variant()),
            ("CalendarDisabled", true.to_variant()),
            ("ContactsDisabled", false.to_variant()),
        ]);
        let ifaces = iface_dict(&[
            (ACCOUNT_IFACE, account),
            ("org.gnome.OnlineAccounts.Mail", glib::VariantDict::new(None).to_variant()),
            ("org.gnome.OnlineAccounts.Contacts", glib::VariantDict::new(None).to_variant()),
            ("org.gnome.OnlineAccounts.OAuth2", glib::VariantDict::new(None).to_variant()),
        ]);
        let path = ObjectPath::try_from("/org/gnome/OnlineAccounts/Accounts/account_1").unwrap();
        let objects = Variant::array_from_iter_with_type(
            VariantTy::new("{oa{sa{sv}}}").unwrap(),
            [Variant::from_dict_entry(&path.to_variant(), &ifaces)],
        );
        let list = accounts_from_managed_objects(&objects);
        assert_eq!(list.len(), 1);
        let a = &list[0];
        assert_eq!(a.provider_type, "google");
        assert_eq!(a.label(), "a@example.com");
        assert!(!a.attention_needed);
        let mail = a.services.iter().find(|s| s.service == Service::Mail).unwrap();
        assert!(mail.enabled);
        assert!(mail.can_toggle);
        let cal = a.services.iter().find(|s| s.service == Service::Calendar).unwrap();
        assert!(!cal.enabled);
        assert!(cal.can_toggle);
        let contacts = a.services.iter().find(|s| s.service == Service::Contacts).unwrap();
        assert!(contacts.enabled);
        assert!(a.services.iter().all(|s| s.service != Service::Files));
        assert!(a.services.iter().all(|s| s.service != Service::Unknown("OAuth2".into())));
    }
}
