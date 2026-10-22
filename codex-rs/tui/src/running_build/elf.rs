//! Bounded reads of GNU build-ID notes from ELF32/ELF64 program headers.
//! Never load, mmap, or execute the candidate, which can be a large or incomplete build.

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;

fn number(bytes: &[u8], offset: usize, width: usize, little: bool) -> u64 {
    let bytes = &bytes[offset..offset + width];
    if little {
        bytes.iter().rev().fold(0, |n, b| (n << 8) | u64::from(*b))
    } else {
        bytes.iter().fold(0, |n, b| (n << 8) | u64::from(*b))
    }
}

pub(super) fn read_build_id(file: &File) -> io::Result<Option<Vec<u8>>> {
    let size = file.metadata()?.len();
    if size < 64 {
        return Ok(None);
    }
    let mut header = [0; 64];
    file.read_exact_at(&mut header, /*offset*/ 0)?;
    if &header[..4] != b"\x7fELF"
        || !matches!(header[4], 1 | 2)
        || !matches!(header[5], 1 | 2)
        || header[6] != 1
    {
        return Ok(None);
    }
    let is64 = header[4] == 2;
    let little = header[5] == 1;
    let (header_size, program_size, phoff_at, phsize_at, phnum_at) = if is64 {
        (64, 56, 32, 54, 56)
    } else {
        (52, 32, 28, 42, 44)
    };
    let width = if is64 { 8 } else { 4 };
    if !matches!(number(&header, /*offset*/ 16, /*width*/ 2, little), 2 | 3)
        || number(&header, /*offset*/ 20, /*width*/ 4, little) != 1
        || number(
            &header,
            if is64 { 52 } else { 40 },
            /*width*/ 2,
            little,
        ) != header_size
    {
        return Ok(None);
    }
    let phoff = number(&header, phoff_at, width, little);
    let phsize = number(&header, phsize_at, /*width*/ 2, little);
    let phnum = number(&header, phnum_at, /*width*/ 2, little);
    if phoff < header_size
        || phsize != program_size
        || !(1..=256).contains(&phnum)
        || phoff > size
        || phnum * phsize > size - phoff
    {
        return Ok(None);
    }
    let mut id = None;
    for index in 0..phnum {
        let mut program = [0; 56];
        file.read_exact_at(&mut program[..phsize as usize], phoff + index * phsize)?;
        let offset = number(&program, if is64 { 8 } else { 4 }, width, little);
        let length = number(&program, if is64 { 32 } else { 16 }, width, little);
        if offset > size || length > size - offset {
            return Ok(None);
        }
        if number(&program, /*offset*/ 0, /*width*/ 4, little) == 4
            && (12..=65536).contains(&length)
        {
            let mut notes = vec![0; length as usize];
            file.read_exact_at(&mut notes, offset)?;
            let mut pos = 0;
            while pos + 12 <= notes.len() {
                let name_size = number(&notes, pos, /*width*/ 4, little) as usize;
                let desc_size = number(&notes, pos + 4, /*width*/ 4, little) as usize;
                let kind = number(&notes, pos + 8, /*width*/ 4, little);
                pos += 12;
                let Some(name_space) = name_size.checked_add(3).map(|n| n & !3) else {
                    return Ok(None);
                };
                let Some(desc_space) = desc_size.checked_add(3).map(|n| n & !3) else {
                    return Ok(None);
                };
                if name_space > notes.len() - pos || desc_space > notes.len() - pos - name_space {
                    return Ok(None);
                }
                if kind == 3
                    && name_size == 4
                    && &notes[pos..pos + 4] == b"GNU\0"
                    && (1..=256).contains(&desc_size)
                {
                    id = Some(notes[pos + name_space..pos + name_space + desc_size].to_vec());
                }
                pos += name_space + desc_space;
            }
        }
    }
    Ok(id)
}
