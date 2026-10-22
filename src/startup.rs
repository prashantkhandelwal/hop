use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegGetValueW, RegOpenKeyExW, RegSetValueExW,
};
use windows::core::{PCWSTR, w};

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE: PCWSTR = w!("Hop");

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        if let Err(error) = unsafe { RegCloseKey(self.0) }.ok() {
            eprintln!("Hop: Cannot close startup registry key: {error}");
        }
    }
}

pub fn enabled() -> Result<bool, String> {
    read_command(RUN_KEY).map(|command| command.is_some())
}

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    let command = if enabled {
        let executable = std::env::current_exe()
            .map_err(|error| format!("Cannot locate Hop for Windows startup: {error}"))?;
        Some(startup_command(&executable)?)
    } else {
        None
    };
    write_command(RUN_KEY, command.as_deref())
}

fn startup_command(executable: &Path) -> Result<Vec<u16>, String> {
    let path: Vec<u16> = executable.as_os_str().encode_wide().collect();
    if !executable.is_absolute() || path.iter().any(|unit| *unit == 0 || *unit == 34) {
        return Err(
            "Windows startup requires an absolute executable path without quotes or NULs.".into(),
        );
    }
    let mut command = vec![34];
    command.extend(path);
    command.extend("\" --watch".encode_utf16());
    if command.len() > 260 {
        return Err(
            "The Hop executable path is too long for Windows startup; move Hop to a shorter path."
                .into(),
        );
    }
    command.push(0);
    Ok(command)
}

fn read_command(subkey: PCWSTR) -> Result<Option<Vec<u16>>, String> {
    let mut length = 0;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey,
            VALUE,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut length),
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    status
        .ok()
        .map_err(|error| format!("Cannot read Windows startup setting: {error}"))?;
    if length < 4 || length % 2 != 0 {
        return Err("The Hop Windows startup entry is empty or malformed.".into());
    }
    let mut command = vec![0u16; length as usize / 2];
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey,
            VALUE,
            RRF_RT_REG_SZ,
            None,
            Some(command.as_mut_ptr().cast()),
            Some(&mut length),
        )
    }
    .ok()
    .map_err(|error| format!("Cannot read Windows startup command: {error}"))?;
    command.truncate(length as usize / 2);
    if length % 2 != 0
        || command.len() < 2
        || command.last() != Some(&0)
        || command[..command.len() - 1].contains(&0)
    {
        return Err("The Hop Windows startup entry is empty or malformed.".into());
    }
    Ok(Some(command))
}

fn write_command(subkey: PCWSTR, command: Option<&[u16]>) -> Result<(), String> {
    let mut key = HKEY::default();
    let status = if command.is_some() {
        unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                subkey,
                None,
                None,
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                None,
                &mut key,
                None,
            )
        }
    } else {
        unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, subkey, None, KEY_SET_VALUE, &mut key) }
    };
    if command.is_none() && status == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    status
        .ok()
        .map_err(|error| format!("Cannot open Windows startup registration: {error}"))?;
    let key = Key(key);
    let status = if let Some(command) = command {
        let bytes: Vec<_> = command.iter().flat_map(|unit| unit.to_le_bytes()).collect();
        unsafe { RegSetValueExW(key.0, VALUE, None, REG_SZ, Some(&bytes)) }
    } else {
        unsafe { RegDeleteValueW(key.0, VALUE) }
    };
    if command.is_none() && status == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    status
        .ok()
        .map_err(|error| format!("Cannot update Windows startup registration: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Registry::RegDeleteTreeW;

    #[test]
    fn startup_command_quotes_paths_and_launches_the_watcher() {
        let command = startup_command(Path::new("C:\\Program Files\\Hop\\hop.exe")).unwrap();
        assert_eq!(
            String::from_utf16(&command[..command.len() - 1]).unwrap(),
            "\"C:\\Program Files\\Hop\\hop.exe\" --watch"
        );
        assert_eq!(command.last(), Some(&0));
        for path in [
            "hop.exe",
            "C:\\bad\"path\\hop.exe",
            "C:\\bad\0path\\hop.exe",
        ] {
            assert!(startup_command(Path::new(path)).is_err());
        }
        assert!(startup_command(Path::new(&format!("C:\\{}\\hop.exe", "x".repeat(260)))).is_err());
    }

    #[test]
    fn startup_registration_roundtrips_without_touching_actual_autostart() {
        let directory = tempfile::tempdir().unwrap();
        let name = format!(
            "Software\\Hop\\Tests\\{}",
            directory.path().file_name().unwrap().to_string_lossy()
        );
        let wide: Vec<_> = name.encode_utf16().chain(Some(0)).collect();
        let subkey = PCWSTR(wide.as_ptr());
        struct Cleanup(Vec<u16>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(self.0.as_ptr())) }
                    .ok()
                    .expect("Cannot clean up test-owned startup registry key");
            }
        }
        assert!(read_command(subkey).unwrap().is_none());
        write_command(subkey, None).unwrap();
        assert!(read_command(subkey).unwrap().is_none());
        let first = startup_command(Path::new("C:\\Program Files\\Hop\\hop.exe")).unwrap();
        write_command(subkey, Some(&first)).unwrap();
        let _cleanup = Cleanup(wide.clone());
        assert_eq!(read_command(subkey).unwrap(), Some(first));
        let moved = startup_command(Path::new("D:\\Apps\\Hop\\hop.exe")).unwrap();
        write_command(subkey, Some(&moved)).unwrap();
        assert_eq!(read_command(subkey).unwrap(), Some(moved));
        write_command(subkey, None).unwrap();
        write_command(subkey, None).unwrap();
        assert!(read_command(subkey).unwrap().is_none());
        write_command(subkey, Some(&[0])).unwrap();
        assert!(read_command(subkey).is_err());
    }
}
