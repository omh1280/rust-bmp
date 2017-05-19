extern crate byteorder;

use byteorder::{LittleEndian, ReadBytesExt};

use std::convert::{From, AsRef};
use std::error::Error;
use std::fmt;
use std::io::{self, Cursor, Read, SeekFrom, Seek};

// Import structs/functions defined in lib.rs
use super::*;
use self::BmpErrorKind::*;

/// A result type, either containing an `Image` or a `BmpError`.
pub type BmpResult<T> = Result<T, BmpError>;

/// The error type returned if the decoding of an image from disk fails.
#[derive(Debug)]
pub struct BmpError {
    pub kind: BmpErrorKind,
    pub details: String,
}

impl BmpError {
    fn new<T: AsRef<str>>(kind: BmpErrorKind, details: T) -> BmpError {
        BmpError {
            kind: kind,
            details: String::from(details.as_ref()),
        }
    }
}

impl fmt::Display for BmpError {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match self.kind {
            BmpIoError(ref error) => error.fmt(fmt),
            ref e => {
                let kind_desc: &str = e.as_ref();
                write!(fmt, "{}: {}", kind_desc, self.description())
            }
        }
    }
}

impl From<io::Error> for BmpError {
    fn from(err: io::Error) -> BmpError {
        BmpError::new(BmpIoError(err), "Io Error")
    }
}

impl Error for BmpError {
    fn description(&self) -> &str {
        match self.kind {
            BmpIoError(ref e) => e.description(),
            _ => &self.details
        }
    }
}

/// The different kinds of possible BMP errors.
#[derive(Debug)]
pub enum BmpErrorKind {
    WrongMagicNumbers,
    UnsupportedBitsPerPixel,
    UnsupportedCompressionType,
    UnsupportedBmpVersion,
    Other,
    BmpIoError(io::Error),
}

impl AsRef<str> for BmpErrorKind {
    fn as_ref(&self) -> &str {
        match *self {
            WrongMagicNumbers => "Wrong magic numbers",
            UnsupportedBitsPerPixel => "Unsupported bits per pixel",
            UnsupportedCompressionType => "Unsupported compression type",
            UnsupportedBmpVersion => "Unsupported BMP version",
            _ => "BMP Error",
        }
    }
}

pub fn decode_image(bmp_data: &mut Cursor<Vec<u8>>) -> BmpResult<Image> {
    read_bmp_id(bmp_data)?;
    let header = read_bmp_header(bmp_data)?;
    let dib_header = read_bmp_dib_header(bmp_data)?;

    let color_palette = read_color_palette(bmp_data, &dib_header)?;

    let width = dib_header.width.abs() as u32;
    let height = dib_header.height.abs() as u32;
    let padding = width % 4;

    let data = match color_palette {
        Some(ref palette) =>
            read_indexes(bmp_data.get_mut(), &palette, width as usize, height as usize,
                         dib_header.bits_per_pixel, header.pixel_offset as usize)?,
        None => read_pixels(bmp_data, width, height, header.pixel_offset, padding as i64)?
    };

    let image = Image {
        header: header,
        dib_header: dib_header,
        color_palette: color_palette,
        width: width,
        height: height,
        padding: padding,
        data: data
    };

    Ok(image)
}

fn read_bmp_id(bmp_data: &mut Cursor<Vec<u8>>) -> BmpResult<()> {
    let mut bm = [0, 0];
    bmp_data.read(&mut bm)?;

    if bm == b"BM"[..] {
        Ok(())
    } else {
        Err(BmpError::new(WrongMagicNumbers,
            format!("Expected [66, 77], but was {:?}", bm)))
    }
}

fn read_bmp_header(bmp_data: &mut Cursor<Vec<u8>>) -> BmpResult<BmpHeader> {
    let header = BmpHeader {
        file_size:    bmp_data.read_u32::<LittleEndian>()?,
        creator1:     bmp_data.read_u16::<LittleEndian>()?,
        creator2:     bmp_data.read_u16::<LittleEndian>()?,
        pixel_offset: bmp_data.read_u32::<LittleEndian>()?,
    };

    Ok(header)
}

fn read_bmp_dib_header(bmp_data: &mut Cursor<Vec<u8>>) -> BmpResult<BmpDibHeader> {
    let dib_header = BmpDibHeader {
        header_size:    bmp_data.read_u32::<LittleEndian>()?,
        width:          bmp_data.read_i32::<LittleEndian>()?,
        height:         bmp_data.read_i32::<LittleEndian>()?,
        num_planes:     bmp_data.read_u16::<LittleEndian>()?,
        bits_per_pixel: bmp_data.read_u16::<LittleEndian>()?,
        compress_type:  bmp_data.read_u32::<LittleEndian>()?,
        data_size:      bmp_data.read_u32::<LittleEndian>()?,
        hres:           bmp_data.read_i32::<LittleEndian>()?,
        vres:           bmp_data.read_i32::<LittleEndian>()?,
        num_colors:     bmp_data.read_u32::<LittleEndian>()?,
        num_imp_colors: bmp_data.read_u32::<LittleEndian>()?,
    };

    match dib_header.header_size {
        // BMPv2 has a header size of 12 bytes
        12 => return Err(BmpError::new(UnsupportedBmpVersion, BmpVersion::Two)),
        // BMPv3 has a header size of 40 bytes, it is NT if the compression type is 3
        40 if dib_header.compress_type == 3 =>
            return Err(BmpError::new(UnsupportedBmpVersion, BmpVersion::ThreeNT)),
        // BMPv4 has more data in its header, it is currently ignored but we still try to parse it
        108 | _ => ()
    }

    match dib_header.bits_per_pixel {
        // Currently supported
        1 | 4 | 8 | 24 => (),
        other => return Err(
            BmpError::new(UnsupportedBitsPerPixel, format!("{}", other))
        )
    }

    match CompressionType::from_u32(dib_header.compress_type) {
        CompressionType::Uncompressed => (),
        other => return Err(BmpError::new(UnsupportedCompressionType, other)),
    }

    Ok(dib_header)
}

fn read_color_palette(bmp_data: &mut Cursor<Vec<u8>>, dh: &BmpDibHeader) ->
                      BmpResult<Option<Vec<Pixel>>> {
    let num_entries = match dh.bits_per_pixel {
        // We have a color_palette if there if num_colors in the dib header is not zero
        _ if dh.num_colors != 0 => dh.num_colors as usize,
        // Or if there are 8 or less bits per pixel
        bpp @ 1 | bpp @ 4 | bpp @ 8 => 1 << bpp,
        _ => return Ok(None)
    };

    let num_bytes = match dh.header_size {
        // Each entry in the color_palette is four bytes for Version 3 or 4
        40 | 108 => 4,
        // Three bytes for Version two. Though, this is currently not supported
        _ => return Err(BmpError::new(UnsupportedBmpVersion, BmpVersion::Two))
    };

    let mut px = &mut [0; 4][0 .. num_bytes as usize];
    let mut color_palette = Vec::with_capacity(num_entries);
    for _ in 0 .. num_entries {
        bmp_data.read(&mut px)?;
        color_palette.push(px!(px[2], px[1], px[0]));
    }

    Ok(Some(color_palette))
}

fn read_indexes(bmp_data: &mut Vec<u8>, palette: &Vec<Pixel>,
                width: usize, height: usize, bpp: u16, offset: usize) -> BmpResult<Vec<Pixel>> {
    let mut data = Vec::with_capacity(height * width);
    // Number of bytes to read from each row, varies based on bits_per_pixel
    let bytes_per_row = (width as f64 / (8.0 / bpp as f64)).ceil() as usize;
    for y in 0 .. height {
        let padding = match bytes_per_row % 4 {
            0 => 0,
            other => 4 - other
        };
        let start = offset + (bytes_per_row + padding) * y;
        let bytes = &bmp_data[start .. start + bytes_per_row];

        for i in bit_index(&bytes, bpp as usize, width as usize) {
            data.push(palette[i]);
        }
    }
    Ok(data)
}

fn read_pixels(bmp_data: &mut Cursor<Vec<u8>>, width: u32, height: u32,
               offset: u32, padding: i64) -> BmpResult<Vec<Pixel>> {
    let mut data = Vec::with_capacity((height * width) as usize);
    // seek until data
    let mut vec = bmp_data.clone().into_inner();
    //bmp_data.seek(SeekFrom::Start(offset as u64))?;
    // read pixels until padding
    let mut pad_accum = 0;
    let mut px_idx: usize = 0;
    for y in 0 .. height {
        for x in 0 .. width {
            px_idx = (offset + pad_accum + (x + y * width) * 3) as usize;
            data.push(px!(vec[px_idx + 2], vec[px_idx + 1], vec[px_idx]));
            // bmp_data.read(&mut px)?;
            // data.push(px!(px[2], px[1], px[0]));
        }
        pad_accum += padding as u32;
        // seek padding
        // bmp_data.seek(SeekFrom::Current(padding))?;
    }
    Ok(data)
}

const BITS: usize = 8;

#[derive(Debug)]
struct BitIndex<'a> {
    size: usize,
    nbits: usize,
    bits_left: usize,
    mask: u8,
    bytes: &'a [u8],
    index: usize,
}

fn bit_index<'a>(bytes: &'a [u8], nbits: usize, size: usize) -> BitIndex {
    let bits_left = BITS - nbits;
    BitIndex {
        size: size,
        nbits: nbits,
        bits_left: bits_left,
        mask: (!0 as u8 >> bits_left),
        bytes: bytes,
        index: 0,
    }
}

impl<'a> Iterator for BitIndex<'a> {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        let n = self.index / BITS;
        let offset = self.bits_left - self.index % BITS;

        self.index += self.nbits;

        if self.size == 0 {
            None
        } else {
            self.size -= 1;
            self.bytes.get(n).map(|&block|
                ((block & self.mask << offset) >> offset) as usize
            )
        }
    }
}

#[test]
fn test_calculate_bit_index() {
    let bytes = vec![0b1000_0001, 0b1111_0001];

    let mut bi = bit_index(&bytes, 1, 15);
    assert_eq!(bi.next(), Some(1));
    assert_eq!(bi.next(), Some(0));
    assert_eq!(bi.next(), Some(0));
    assert_eq!(bi.next(), Some(0));
    assert_eq!(bi.next(), Some(0));
    assert_eq!(bi.next(), Some(0));
    assert_eq!(bi.next(), Some(0));
    assert_eq!(bi.next(), Some(1));
    assert_eq!(bi.next(), Some(1));
    assert_eq!(bi.next(), Some(1));
    assert_eq!(bi.next(), Some(1));
    assert_eq!(bi.next(), Some(1));
    assert_eq!(bi.next(), Some(0));
    assert_eq!(bi.next(), Some(0));
    assert_eq!(bi.next(), Some(0));
    assert_eq!(bi.next(), None);
    assert_eq!(bi.next(), None);

    let mut bi = bit_index(&bytes, 4, 4);
    assert_eq!(bi.next(), Some(0b1000));
    assert_eq!(bi.next(), Some(0b0001));
    assert_eq!(bi.next(), Some(0b1111));
    assert_eq!(bi.next(), Some(0b0001));
    assert_eq!(bi.next(), None);

    let mut bi = bit_index(&bytes, 8, 2);
    assert_eq!(bi.next(), Some(0b1000_0001));
    assert_eq!(bi.next(), Some(0b1111_0001));
    assert_eq!(bi.next(), None);
}
