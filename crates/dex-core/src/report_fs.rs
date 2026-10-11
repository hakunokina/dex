//! Host report writes: hold the directory chain and publish the still-open temp by handle.
//! Native relative opens also avoid re-resolving a parent if its reparse data changes.
use std::path::Path;

#[cfg(not(windows))]
pub(super) fn write(
    _root: &Path,
    _dirs: [&str; 2],
    _name: &str,
    _body: &str,
) -> Result<(), String> {
    Err("rapor dosyası güvenli host yazımı bu platformda desteklenmiyor; resultPath kullan".into())
}

#[cfg(windows)]
pub(super) fn write(root: &Path, dirs: [&str; 2], name: &str, body: &str) -> Result<(), String> {
    windows::write(root, dirs, name, body).map_err(|e| format!("rapor dosyası: {e}"))
}

#[cfg(windows)]
mod windows {
    use std::os::windows::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawHandle, FromRawHandle},
    };
    use std::{
        ffi::{c_void, OsStr},
        fs::{File, OpenOptions},
        io::{self, Write},
        mem,
        path::{Component, Path, PathBuf},
        ptr,
    };
    use windows_sys::Win32::Foundation::{HANDLE, UNICODE_STRING};

    const READ_ATTRIBUTES: u32 = 0x80;
    const TRAVERSE: u32 = 0x20;
    const SYNCHRONIZE: u32 = 0x100000;
    const DELETE: u32 = 0x10000;
    const GENERIC_WRITE: u32 = 0x40000000;
    const DIRECTORY_SHARE: u32 = 1; // No write or delete sharing for the entire directory chain.
    const REPARSE: u32 = 0x400;
    const BACKUP_SEMANTICS: u32 = 0x02000000;
    const OPEN_REPARSE_POINT: u32 = 0x00200000;

    // These ABI layouts and functions are the existing Windows API; no dependency/features added.
    #[repr(C)]
    struct ObjectAttributes {
        length: u32,
        root: HANDLE,
        name: *const UNICODE_STRING,
        attributes: u32,
        security_descriptor: *const c_void,
        security_qos: *const c_void,
    }
    #[repr(C)]
    struct IoStatus {
        status: usize,
        information: usize,
    }
    #[repr(C)]
    struct RenameInfo {
        replace: u8,
        root: HANDLE,
        length: u32,
        name: [u16; 64],
    }

    #[link(name = "ntdll")]
    extern "system" {
        fn NtCreateFile(
            handle: *mut HANDLE,
            access: u32,
            attributes: *const ObjectAttributes,
            status: *mut IoStatus,
            allocation: *const i64,
            file_attributes: u32,
            share: u32,
            disposition: u32,
            options: u32,
            ea: *const c_void,
            ea_length: u32,
        ) -> i32;
        fn RtlNtStatusToDosError(status: i32) -> u32;
        fn NtSetInformationFile(
            handle: HANDLE,
            status: *mut IoStatus,
            info: *const c_void,
            length: u32,
            class: i32,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetFinalPathNameByHandleW(
            handle: HANDLE,
            path: *mut u16,
            length: u32,
            flags: u32,
        ) -> u32;
        fn SetFileInformationByHandle(
            handle: HANDLE,
            class: i32,
            info: *const c_void,
            length: u32,
        ) -> i32;
    }

    fn unsafe_dir() -> io::Error {
        io::Error::other("güvenli dizin gerekli (link/dosya/yönlendirme)")
    }

    fn directory(file: &File) -> io::Result<()> {
        let m = file.metadata()?;
        if !m.is_dir() || m.file_attributes() & REPARSE != 0 {
            return Err(unsafe_dir());
        }
        Ok(())
    }

    /// Single-component opens anchored to an already verified handle, never the ambient cwd.
    fn relative(
        parent: &File,
        name: &OsStr,
        access: u32,
        share: u32,
        disposition: u32,
        kind: u32,
    ) -> io::Result<File> {
        let mut wide: Vec<u16> = name.encode_wide().collect();
        if wide.is_empty() || wide.len() > 32766 || wide.iter().any(|c| [0, 47, 92, 58].contains(c))
        {
            return Err(unsafe_dir());
        }
        let text = UNICODE_STRING {
            Length: (wide.len() * 2) as u16,
            MaximumLength: (wide.len() * 2) as u16,
            Buffer: wide.as_mut_ptr(),
        };
        let attrs = ObjectAttributes {
            length: mem::size_of::<ObjectAttributes>() as u32,
            root: parent.as_raw_handle(),
            name: &text,
            attributes: 0x40 | 0x1000, // OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE
            security_descriptor: ptr::null(),
            security_qos: ptr::null(),
        };
        let mut status = IoStatus {
            status: 0,
            information: 0,
        };
        let mut handle = ptr::null_mut();
        // SAFETY: live parent; correctly aligned ABI structs/buffers live for this synchronous call.
        let result = unsafe {
            NtCreateFile(
                &mut handle,
                access | SYNCHRONIZE,
                &attrs,
                &mut status,
                ptr::null(),
                0x80,
                share,
                disposition,
                kind | 0x20 | OPEN_REPARSE_POINT,
                ptr::null(),
                0,
            )
        };
        if result < 0 {
            // SAFETY: pure NTSTATUS conversion.
            return Err(io::Error::from_raw_os_error(
                unsafe { RtlNtStatusToDosError(result) } as i32,
            ));
        }
        // SAFETY: a successful synchronous create returns a new owned handle, closed by File.
        Ok(unsafe { File::from_raw_handle(handle) })
    }

    fn final_path(file: &File) -> io::Result<PathBuf> {
        // SAFETY: live file handle; NULL/zero queries the required UTF-16 buffer size.
        let size =
            unsafe { GetFinalPathNameByHandleW(file.as_raw_handle(), ptr::null_mut(), 0, 0) };
        if size == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut wide = vec![0; size as usize];
        // SAFETY: allocated writable buffer of the advertised size and a live handle.
        let length =
            unsafe { GetFinalPathNameByHandleW(file.as_raw_handle(), wide.as_mut_ptr(), size, 0) };
        if length == 0 {
            return Err(io::Error::last_os_error());
        }
        if length >= size {
            return Err(unsafe_dir());
        }
        use std::os::windows::ffi::OsStringExt;
        Ok(std::ffi::OsString::from_wide(&wide[..length as usize]).into())
    }

    fn chain(root: &Path) -> io::Result<Vec<File>> {
        let mut parts = root.components();
        let Some(Component::Prefix(prefix)) = parts.next() else {
            return Err(unsafe_dir());
        };
        use std::path::Prefix;
        if !matches!(
            prefix.kind(),
            Prefix::Disk(_)
                | Prefix::VerbatimDisk(_)
                | Prefix::UNC(_, _)
                | Prefix::VerbatimUNC(_, _)
        ) {
            return Err(unsafe_dir());
        }
        if parts.next() != Some(Component::RootDir) {
            return Err(unsafe_dir());
        }
        let mut anchor = PathBuf::from(prefix.as_os_str());
        anchor.push(r"\");
        let file = OpenOptions::new()
            .access_mode(READ_ATTRIBUTES | TRAVERSE | SYNCHRONIZE)
            .share_mode(DIRECTORY_SHARE)
            .custom_flags(BACKUP_SEMANTICS | OPEN_REPARSE_POINT)
            .open(anchor)?;
        directory(&file)?;
        let mut held = vec![file];
        for part in parts {
            let Component::Normal(name) = part else {
                return Err(unsafe_dir());
            };
            let file = relative(
                held.last().unwrap(),
                name,
                READ_ATTRIBUTES | TRAVERSE,
                DIRECTORY_SHARE,
                1,
                1,
            )
            .map_err(|e| io::Error::other(format!("güvenli dizin açılamadı: {e}")))?;
            directory(&file)?;
            held.push(file);
        }
        let actual = final_path(held.last().unwrap())?;
        if crate::projects::path_key(&actual.to_string_lossy())
            != crate::projects::path_key(&root.to_string_lossy())
        {
            return Err(unsafe_dir());
        }
        Ok(held)
    }

    fn publish(file: &File, name: &str) -> io::Result<()> {
        let wide: Vec<u16> = name.encode_utf16().collect();
        if wide.len() >= 64 {
            return Err(unsafe_dir());
        }
        // NULL RootDirectory + a simple name renames within the source handle's own parent.
        // Supplying the directory handle instead would reopen it for write and conflict with
        // our no-write-sharing lock (IopOpenLinkOrRenameTarget).
        let mut info = RenameInfo {
            replace: 1,
            root: ptr::null_mut(),
            length: (wide.len() * 2) as u32,
            name: [0; 64],
        };
        info.name[..wide.len()].copy_from_slice(&wide);
        let length = mem::offset_of!(RenameInfo, name) + wide.len() * 2;
        let mut status = IoStatus {
            status: 0,
            information: 0,
        };
        // SAFETY: initialized ABI struct, bounded simple UTF-16 name, live source handle.
        let result = unsafe {
            NtSetInformationFile(
                file.as_raw_handle(),
                &mut status,
                (&info as *const RenameInfo).cast(),
                length as u32,
                10,
            )
        };
        if result < 0 {
            // SAFETY: pure NTSTATUS conversion.
            let error =
                io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(result) } as i32);
            return Err(io::Error::other(format!("atomik taşınamadı: {error}")));
        }
        Ok(())
    }

    fn discard(file: &File) -> io::Result<()> {
        // FILE_DISPOSITION_INFO.DeleteFile; deletion occurs when this handle closes.
        let delete: u8 = 1;
        // SAFETY: live DELETE-capable handle and initialized one-byte FILE_DISPOSITION_INFO.
        if unsafe {
            SetFileInformationByHandle(file.as_raw_handle(), 4, (&delete as *const u8).cast(), 1)
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(super) fn write(root: &Path, dirs: [&str; 2], name: &str, body: &str) -> io::Result<()> {
        let mut held = chain(root)?;
        for name in dirs {
            let file = relative(
                held.last().unwrap(),
                OsStr::new(name),
                READ_ATTRIBUTES | TRAVERSE,
                DIRECTORY_SHARE,
                3,
                1,
            )
            .map_err(|e| io::Error::other(format!("güvenli dizin açılamadı: {e}")))?;
            directory(&file)?;
            held.push(file);
        }
        let parent = held.last().unwrap();
        match relative(parent, OsStr::new(name), READ_ATTRIBUTES, 7, 1, 0) {
            Ok(file) => {
                let m = file.metadata()?;
                if !m.is_file() || m.file_attributes() & REPARSE != 0 {
                    return Err(unsafe_dir());
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        #[cfg(test)]
        super::super::report_io_hook("directories", &root.join(dirs[0]).join(dirs[1]));
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce)
            .map_err(|_| io::Error::other("güvenli geçici ad üretilemedi"))?;
        let temp = format!(
            ".report-{}.tmp",
            nonce.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        #[cfg(test)]
        let temp_path = root.join(dirs[0]).join(dirs[1]).join(&temp);
        #[cfg(test)]
        super::super::report_io_hook("temp_name", &temp_path);
        let mut out = relative(
            parent,
            OsStr::new(&temp),
            GENERIC_WRITE | DELETE,
            0,
            2,
            0x40,
        )?;
        let result = (|| {
            out.write_all(body.as_bytes())?;
            out.sync_all()?;
            #[cfg(test)]
            super::super::report_io_hook("written", &temp_path);
            #[cfg(test)]
            super::super::report_io_hook("publish", &temp_path);
            publish(&out, name)
        })();
        if let Err(error) = result {
            return match discard(&out) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(io::Error::other(format!(
                    "{error}; geçici rapor dosyası temizlenemedi: {cleanup}"
                ))),
            };
        }
        // out and every ancestor stay alive through sync, publication and error cleanup.
        Ok(())
    }
}
