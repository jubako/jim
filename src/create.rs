use jubako as jbk;

use jbk::creator::schema;
use mime_guess::mime;
use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;

const VENDOR_ID: u32 = 0x6a_69_6d_00;

#[derive(Debug)]
enum EntryKind {
    Dir,
    File,
    Link,
    Other,
}

#[derive(Debug)]
pub struct Entry {
    kind: EntryKind,
    path: PathBuf,
}

impl Entry {
    pub fn new(path: PathBuf) -> jbk::Result<Self> {
        let attr = fs::symlink_metadata(&path)?;
        Ok(if attr.is_dir() {
            Self {
                kind: EntryKind::Dir,
                path,
            }
        } else if attr.is_file() {
            Self {
                kind: EntryKind::File,
                path,
            }
        } else if attr.is_symlink() {
            Self {
                kind: EntryKind::Link,
                path,
            }
        } else {
            Self {
                kind: EntryKind::Other,
                path,
            }
        })
    }

    pub fn new_from_fs(dir_entry: fs::DirEntry) -> Self {
        let path = dir_entry.path();
        if let Ok(file_type) = dir_entry.file_type() {
            if file_type.is_dir() {
                Self {
                    kind: EntryKind::Dir,
                    path,
                }
            } else if file_type.is_file() {
                Self {
                    kind: EntryKind::File,
                    path,
                }
            } else if file_type.is_symlink() {
                Self {
                    kind: EntryKind::Link,
                    path,
                }
            } else {
                Self {
                    kind: EntryKind::Other,
                    path,
                }
            }
        } else {
            Self {
                kind: EntryKind::Other,
                path,
            }
        }
    }
}

pub struct Creator {
    content_pack: jbk::creator::ContentPackCreator,
    directory_pack: jbk::creator::DirectoryPackCreator,
    entry_store: Box<jbk::creator::EntryStore<jbk::creator::BasicEntry>>,
    entry_count: jbk::EntryCount,
    main_entry_path: PathBuf,
    main_entry_id: jbk::EntryIdx,
}

impl Creator {
    pub fn new<P: AsRef<Path>>(outfile: P, main_entry: PathBuf) -> jbk::Result<Self> {
        let outfile = outfile.as_ref();
        let mut outfilename: OsString = outfile.file_name().unwrap().to_os_string();
        outfilename.push(".jimc");
        let mut content_pack_path = PathBuf::new();
        content_pack_path.push(outfile);
        content_pack_path.set_file_name(outfilename);
        let content_pack = jbk::creator::ContentPackCreator::new(
            content_pack_path,
            jbk::PackId::from(1),
            VENDOR_ID,
            jbk::FreeData40::clone_from_slice(&[0x00; 40]),
            jbk::CompressionType::Zstd,
        )?;

        outfilename = outfile.file_name().unwrap().to_os_string();
        outfilename.push(".jimd");
        let mut directory_pack_path = PathBuf::new();
        directory_pack_path.push(outfile);
        directory_pack_path.set_file_name(outfilename);
        let mut directory_pack = jbk::creator::DirectoryPackCreator::new(
            directory_pack_path,
            jbk::PackId::from(0),
            VENDOR_ID,
            jbk::FreeData31::clone_from_slice(&[0x00; 31]),
        );

        let path_store = directory_pack.create_value_store(jbk::creator::ValueStoreKind::Plain);
        let mime_store = directory_pack.create_value_store(jbk::creator::ValueStoreKind::Indexed);

        let schema = schema::Schema::new(
            // Common part
            schema::CommonProperties::new(vec![
                schema::Property::new_array(1, Rc::clone(&path_store)), // the path
            ]),
            vec![
                // Content
                schema::VariantProperties::new(vec![
                    schema::Property::new_array(0, Rc::clone(&mime_store)), // the mimetype
                    schema::Property::new_content_address(),
                ]),
                // Redirect
                schema::VariantProperties::new(vec![
                    schema::Property::new_array(0, Rc::clone(&path_store)), // Id of the linked entry
                ]),
            ],
            Some(vec![0.into()]),
        );

        let entry_store = Box::new(jbk::creator::EntryStore::new(schema));

        Ok(Self {
            content_pack,
            directory_pack,
            entry_store,
            entry_count: 0.into(),
            main_entry_path: main_entry,
            main_entry_id: Default::default(),
        })
    }

    fn finalize(mut self, outfile: PathBuf) -> jbk::Result<()> {
        let entry_store_id = self.directory_pack.add_entry_store(self.entry_store);
        self.directory_pack.create_index(
            "jim_entries",
            jubako::ContentAddress::new(0.into(), 0.into()),
            jbk::PropertyIdx::from(0),
            entry_store_id,
            self.entry_count,
            jubako::EntryIdx::from(0).into(),
        );
        self.directory_pack.create_index(
            "jim_main",
            jubako::ContentAddress::new(0.into(), 0.into()),
            jbk::PropertyIdx::from(0),
            entry_store_id,
            jubako::EntryCount::from(1),
            self.main_entry_id.into(),
        );
        let directory_pack_info = self.directory_pack.finalize()?;
        let content_pack_info = self.content_pack.finalize()?;
        let mut manifest_creator = jbk::creator::ManifestPackCreator::new(
            outfile,
            VENDOR_ID,
            jbk::FreeData63::clone_from_slice(&[0x00; 63]),
        );

        manifest_creator.add_pack(directory_pack_info);
        manifest_creator.add_pack(content_pack_info);
        manifest_creator.finalize()?;
        Ok(())
    }

    pub fn run(mut self, outfile: PathBuf, infiles: Vec<PathBuf>) -> jbk::Result<()> {
        for infile in infiles {
            let entry = Entry::new(infile)?;
            self.handle(entry)?;
        }
        self.finalize(outfile)
    }

    fn handle(&mut self, entry: Entry) -> jbk::Result<()> {
        if self.entry_count.into_u32() % 1000 == 0 {
            println!("{} {:?}", self.entry_count, entry);
        }
        let entry_path = entry.path.clone();

        let mut value_entry_path = entry.path.clone().into_os_string().into_vec();
        value_entry_path.truncate(255);
        let value_entry_path = jbk::Value::Array(value_entry_path);
        let new_entry = match entry.kind {
            EntryKind::Dir => {
                for sub_entry in fs::read_dir(&entry.path)? {
                    self.handle(Entry::new_from_fs(sub_entry?))?;
                }
                None
            }
            EntryKind::File => {
                let file = jbk::Reader::from(jbk::creator::FileSource::open(&entry.path)?);

                let mime_type = match mime_guess::from_path(entry.path).first() {
                    Some(m) => m,
                    None => {
                        let mut buf = [0u8; 100];
                        let size = std::cmp::min(100, file.size().into_usize());
                        file.create_flux_to(jbk::End::new_size(size))
                            .read_exact(&mut buf[..size])?;
                        (|| {
                            for window in buf[..size].windows(4) {
                                if window == b"html" {
                                    return mime::TEXT_HTML;
                                }
                            }
                            mime::APPLICATION_OCTET_STREAM
                        })()
                    }
                };
                let content_id = self.content_pack.add_content(file)?;

                Some(jbk::creator::BasicEntry::new_from_schema(
                    &self.entry_store.schema,
                    Some(0.into()),
                    vec![
                        value_entry_path,
                        jbk::Value::Array(mime_type.to_string().into()),
                        jbk::Value::Content(jbk::ContentAddress::new(
                            jbk::PackId::from(1),
                            content_id,
                        )),
                    ],
                ))
            }
            EntryKind::Link => {
                let mut target = fs::read_link(&entry.path)?.into_os_string().into_vec();
                target.truncate(255);
                Some(jbk::creator::BasicEntry::new_from_schema(
                    &self.entry_store.schema,
                    Some(1.into()),
                    vec![value_entry_path, jbk::Value::Array(target)],
                ))
            }
            EntryKind::Other => unreachable!(),
        };
        if let Some(e) = new_entry {
            if entry_path == self.main_entry_path {
                self.main_entry_id = self.entry_count.into_u32().into();
            }
            self.entry_store.add_entry(e);
            self.entry_count += 1;
        }
        Ok(())
    }
}
