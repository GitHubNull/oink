//! Publication targets for one X11 ownership. Building a binding never reads
//! payload bytes; provider-backed targets stay lazy until a peer requests them.
use crate::content::{self, url};
use crate::formats;
use crate::linux::atoms;
use crate::linux::virtual_files;
use crate::model::{Inventory, NativeFormat, Representation, RepresentationData, StandardFormat, Unavailable};
use crate::{ClipData, ClipboardError, NativeSpec, Sensitivity};
use oink_foundation::clipboard::ClipPlatform;
use std::io;
use std::sync::Arc;

#[derive(Clone)]
pub(crate) enum Source {
    Bytes(Arc<Vec<u8>>),
    Lazy(Arc<dyn Fn() -> io::Result<Vec<u8>> + Send + Sync>),
}

#[derive(Default)]
pub(crate) struct Binding {
    pub(crate) targets: Vec<(String, Source)>,
}

impl Binding {
    pub(super) fn add(&mut self, name: &str, source: Source) {
        if !self.targets.iter().any(|(existing, _)| existing == name) {
            self.targets.push((name.to_owned(), source));
        }
    }
}

/// Binding plus the atom table interned for one ownership.
pub(crate) struct Installed {
    pub(crate) binding: Binding,
    pub(crate) names: std::collections::HashMap<u64, String>,
    pub(crate) atoms: Vec<u64>,
}

impl Binding {
    pub(crate) fn install(self, intern: impl Fn(&str) -> io::Result<u64>) -> io::Result<Installed> {
        let mut names = std::collections::HashMap::new();
        let mut atoms = Vec::with_capacity(self.targets.len());
        for (name, _) in &self.targets {
            let atom = intern(name)?;
            if atom == 0 {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "target atom is absent"));
            }
            names.insert(atom, name.clone());
            atoms.push(atom);
        }
        Ok(Installed {
            binding: self,
            names,
            atoms,
        })
    }
}

impl Installed {
    pub(crate) fn source(&self, atom: u64) -> Option<&Source> {
        let name = self.names.get(&atom)?;
        self.binding
            .targets
            .iter()
            .find_map(|(existing, source)| (existing == name).then_some(source))
    }
}

impl std::fmt::Debug for Binding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Binding").field("targets", &self.targets.len()).finish()
    }
}

/// Local formats after the documented cross-platform conversion.
pub(crate) fn from_items(items: &[ClipData], sensitivity: Sensitivity) -> Result<Binding, ClipboardError> {
    let mut binding = Binding::default();
    for item in formats::for_platform(items, ClipPlatform::Linux, sensitivity) {
        match item {
            ClipData::Text(text) => add_text(&mut binding, &text),
            ClipData::Html(html) => {
                let bytes = Arc::new(html.into_bytes());
                for name in atoms::HTML_TARGETS {
                    binding.add(name, Source::Bytes(bytes.clone()));
                }
            }
            ClipData::Rtf(bytes) => {
                let bytes = Arc::new(bytes);
                for name in atoms::RTF_TARGETS {
                    binding.add(name, Source::Bytes(bytes.clone()));
                }
            }
            ClipData::Png(bytes) => binding.add(atoms::IMAGE_PNG, Source::Bytes(Arc::new(bytes))),
            ClipData::Custom {
                platform: ClipPlatform::Linux,
                name,
                bytes,
            } => add_custom(&mut binding, &name, bytes),
            _ => {}
        }
    }
    for item in items {
        if let ClipData::Files(entries) = item {
            virtual_files::add(&mut binding, &virtual_files::top_paths(entries, None));
        }
    }
    if binding.targets.is_empty() {
        return Err(ClipboardError::Platform("没有可写入的内容".into()));
    }
    Ok(binding)
}

fn add_text(binding: &mut Binding, text: &str) {
    let bytes = Arc::new(text.as_bytes().to_vec());
    for name in [atoms::UTF8, atoms::TEXT, atoms::TEXT_PLAIN, atoms::TEXT_PLAIN_UTF8] {
        binding.add(name, Source::Bytes(bytes.clone()));
    }
    if let Some(latin) = atoms::latin1_bytes(text) {
        binding.add(atoms::STRING, Source::Bytes(Arc::new(latin)));
    }
}

fn add_custom(binding: &mut Binding, name: &str, bytes: Vec<u8>) {
    if formats::is_marker(ClipPlatform::Linux, name) {
        if name == formats::LINUX_MARKERS[0] && bytes != formats::LINUX_SECRET {
            return;
        }
        binding.add(name, Source::Bytes(Arc::new(bytes)));
    } else if formats::linux_custom(name) {
        binding.add(name, Source::Bytes(Arc::new(bytes)));
    }
}

/// Remote or foreign inventory re-published on X11. Same-platform bytes stay
/// raw; foreign representations are rendered through the portable contract.
pub(crate) fn from_spec(spec: &NativeSpec) -> Result<Binding, ClipboardError> {
    let mut binding = Binding::default();
    let provider = spec.contents.clone();
    let same = spec.inventory.platform == ClipPlatform::Linux;
    for rep in spec.inventory.wanted(ClipPlatform::Linux) {
        match rep.standard {
            Some(StandardFormat::Text) => add_text_rep(&mut binding, provider.clone(), rep.clone(), same),
            Some(StandardFormat::Html) => add_utf8_rep(&mut binding, provider.clone(), rep.clone(), same, atoms::HTML_TARGETS),
            Some(StandardFormat::Rtf) => add_raw_rep(&mut binding, provider.clone(), rep.clone(), atoms::RTF_TARGETS),
            Some(StandardFormat::Png) => add_raw_rep(&mut binding, provider.clone(), rep.clone(), &[atoms::IMAGE_PNG]),
            Some(StandardFormat::Url) => add_url_rep(&mut binding, provider.clone(), rep.clone(), same),
            Some(StandardFormat::Privacy) => add_privacy(&mut binding, provider.clone(), rep.clone(), same),
            None => {
                if let (true, NativeFormat::LinuxAtom(name)) = (same, &rep.format)
                    && formats::linux_custom(name)
                {
                    binding.add(name, raw_source(provider.clone(), rep.id));
                }
            }
        }
    }
    if !explicit_marker(&spec.inventory) {
        for item in formats::for_platform(&[], ClipPlatform::Linux, spec.sensitivity) {
            if let ClipData::Custom {
                platform: ClipPlatform::Linux,
                name,
                bytes,
            } = item
            {
                add_custom(&mut binding, &name, bytes);
            }
        }
    }
    let share = spec.contents.namespace().and_then(crate::linux::fuse::share_for);
    virtual_files::add(
        &mut binding,
        &virtual_files::top_paths(&spec.files, share.as_deref()),
    );
    if binding.targets.is_empty() {
        return Err(ClipboardError::Unsupported);
    }
    Ok(binding)
}

fn explicit_marker(inventory: &Inventory) -> bool {
    inventory
        .items
        .iter()
        .flat_map(|item| &item.representations)
        .any(|rep| rep.standard == Some(StandardFormat::Privacy))
}

fn raw_source(provider: Arc<dyn content::Provider>, id: crate::model::RepresentationId) -> Source {
    Source::Lazy(Arc::new(move || {
        let mut reader = provider.open(id)?;
        content::read_all(&mut reader)
    }))
}

fn portable_source(provider: Arc<dyn content::Provider>, id: crate::model::RepresentationId) -> Source {
    Source::Lazy(Arc::new(move || {
        let mut reader = content::open_for(provider.as_ref(), id, ClipPlatform::Linux)?;
        content::read_all(&mut reader)
    }))
}

fn decode_text(format: &NativeFormat, bytes: &[u8]) -> io::Result<String> {
    if matches!(format, NativeFormat::LinuxAtom(name) if name == atoms::STRING) {
        return Ok(atoms::latin1(bytes));
    }
    String::from_utf8(bytes.to_vec()).map_err(|_| content::unavailable(Unavailable::InvalidData))
}

fn add_text_rep(binding: &mut Binding, provider: Arc<dyn content::Provider>, rep: Representation, same: bool) {
    let utf8 = if same {
        let format = rep.format.clone();
        let inner = provider.clone();
        let id = rep.id;
        Source::Lazy(Arc::new(move || {
            let mut reader = inner.open(id)?;
            Ok(decode_text(&format, &content::read_all(&mut reader)?)?.into_bytes())
        }))
    } else {
        portable_source(provider.clone(), rep.id)
    };
    for name in [atoms::UTF8, atoms::TEXT, atoms::TEXT_PLAIN, atoms::TEXT_PLAIN_UTF8] {
        binding.add(name, utf8.clone());
    }
    // ISO-8859-1 is only exact for a representation that already is Latin-1.
    if same && matches!(&rep.format, NativeFormat::LinuxAtom(name) if name == atoms::STRING) {
        binding.add(atoms::STRING, raw_source(provider, rep.id));
    }
}

fn add_utf8_rep(
    binding: &mut Binding,
    provider: Arc<dyn content::Provider>,
    rep: Representation,
    same: bool,
    targets: &[&str],
) {
    let source = if same {
        let inner = provider.clone();
        let id = rep.id;
        Source::Lazy(Arc::new(move || {
            let mut reader = inner.open(id)?;
            String::from_utf8(content::read_all(&mut reader)?)
                .map(String::into_bytes)
                .map_err(|_| content::unavailable(Unavailable::InvalidData))
        }))
    } else {
        portable_source(provider.clone(), rep.id)
    };
    for name in targets {
        binding.add(name, source.clone());
    }
}

fn add_raw_rep(
    binding: &mut Binding,
    provider: Arc<dyn content::Provider>,
    rep: Representation,
    targets: &[&str],
) {
    let source = portable_source(provider, rep.id);
    for name in targets {
        binding.add(name, source.clone());
    }
}

fn add_url_rep(binding: &mut Binding, provider: Arc<dyn content::Provider>, rep: Representation, same: bool) {
    let format = rep.format.clone();
    let id = rep.id;
    let source = Source::Lazy(Arc::new(move || {
        let bytes = if same {
            let mut reader = provider.open(id)?;
            content::read_all(&mut reader)?
        } else {
            let mut reader = content::open_for(provider.as_ref(), id, ClipPlatform::Linux)?;
            content::read_all(&mut reader)?
        };
        let link = if same {
            url::native(&format, &bytes)?
        } else {
            url::Link::decode(&bytes)?
        };
        link.render(ClipPlatform::Linux, atoms::URI_LIST)
    }));
    binding.add(atoms::URI_LIST, source);
}

fn add_privacy(binding: &mut Binding, provider: Arc<dyn content::Provider>, rep: Representation, same: bool) {
    if same {
        let NativeFormat::LinuxAtom(name) = &rep.format else {
            return;
        };
        if !formats::is_marker(ClipPlatform::Linux, name) {
            return;
        }
        match &rep.data {
            RepresentationData::Inline(bytes) => {
                if name == formats::LINUX_MARKERS[0] && bytes != formats::LINUX_SECRET {
                    return;
                }
                binding.add(name, Source::Bytes(Arc::new(bytes.clone())));
            }
            _ if name == formats::LINUX_MARKERS[0] => {
                binding.add(name, validating_marker(provider, rep.id, name.clone()));
            }
            _ => binding.add(name, raw_source(provider, rep.id)),
        }
        return;
    }
    // Foreign marker: exact bytes are already inline for captured inventories.
    let (NativeFormat::Mac(name) | NativeFormat::WindowsRegistered(name)) = &rep.format else {
        return;
    };    if let RepresentationData::Inline(bytes) = &rep.data {
        let Some(custom) = foreign_custom(&rep.format, bytes.clone()) else {
            return;
        };
        for item in formats::for_platform(&[custom], ClipPlatform::Linux, Sensitivity::Normal) {
            if let ClipData::Custom {
                platform: ClipPlatform::Linux,
                name,
                bytes,
            } = item
            {
                add_custom(binding, &name, bytes);
            }
        }
        return;
    }
    for target in candidate_markers(&rep.format, name) {
        binding.add(target, mapped_marker(provider.clone(), rep.clone(), target));
    }
}

fn foreign_custom(format: &NativeFormat, bytes: Vec<u8>) -> Option<ClipData> {
    Some(match format {
        NativeFormat::Mac(name) => formats::custom(ClipPlatform::Mac, name, bytes),
        NativeFormat::WindowsRegistered(name) => formats::custom(ClipPlatform::Windows, name, bytes),
        NativeFormat::LinuxAtom(name) => formats::custom(ClipPlatform::Linux, name, bytes),
        NativeFormat::WindowsPredefined(_) | NativeFormat::Unknown(_) => return None,
    })
}

fn candidate_markers(format: &NativeFormat, name: &str) -> Vec<&'static str> {
    use formats::{LINUX_MARKERS, MAC_MARKERS, WIN_MARKERS};
    match format {
        NativeFormat::Mac(_)
            if name == MAC_MARKERS[0] || name == "de.petermaurer.TransientPasteboardType" =>
        {
            vec![LINUX_MARKERS[1]]
        }
        NativeFormat::Mac(_) if name == MAC_MARKERS[1] || name == "com.agilebits.onepassword" => {
            vec![LINUX_MARKERS[0]]
        }
        NativeFormat::Mac(_)
            if name == MAC_MARKERS[2]
                || name == "com.typeit4me.clipping"
                || name == "Pasteboard generator type" =>
        {
            vec![LINUX_MARKERS[2]]
        }
        NativeFormat::WindowsRegistered(_) if name.eq_ignore_ascii_case(WIN_MARKERS[0]) => {
            vec![LINUX_MARKERS[1]]
        }
        NativeFormat::WindowsRegistered(_) if name.eq_ignore_ascii_case(WIN_MARKERS[1]) => {
            vec![LINUX_MARKERS[0]]
        }
        NativeFormat::WindowsRegistered(_) if name.eq_ignore_ascii_case(WIN_MARKERS[2]) => {
            vec![LINUX_MARKERS[0]]
        }
        _ => Vec::new(),
    }
}

fn mapped_marker(provider: Arc<dyn content::Provider>, rep: Representation, target: &'static str) -> Source {
    Source::Lazy(Arc::new(move || {
        let mut reader = content::open_for(provider.as_ref(), rep.id, ClipPlatform::Linux)?;
        let bytes = content::read_all(&mut reader)?;
        let custom = foreign_custom(&rep.format, bytes).ok_or_else(|| content::unavailable(Unavailable::Unsupported))?;
        let mapped = formats::for_platform(&[custom], ClipPlatform::Linux, Sensitivity::Normal);
        mapped
            .into_iter()
            .find_map(|item| match item {
                ClipData::Custom {
                    platform: ClipPlatform::Linux,
                    name,
                    bytes,
                } if name == target => Some(bytes),
                _ => None,
            })
            .ok_or_else(|| content::unavailable(Unavailable::Unsupported))
    }))
}

fn validating_marker(
    provider: Arc<dyn content::Provider>,
    id: crate::model::RepresentationId,
    name: String,
) -> Source {
    Source::Lazy(Arc::new(move || {
        let mut reader = provider.open(id)?;
        let bytes = content::read_all(&mut reader)?;
        if name == formats::LINUX_MARKERS[0] && bytes != formats::LINUX_SECRET {
            return Err(content::unavailable(Unavailable::Unsupported));
        }
        Ok(bytes)
    }))
}
