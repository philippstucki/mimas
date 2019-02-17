use rusqlite::{Connection, NO_PARAMS, OptionalExtension, OpenFlags};
use rusqlite::types::ToSql;
use map::{MapChunkData, MapBlock, CHUNKSIZE};
use StrErr;
use nalgebra::Vector3;
use std::{io, path::Path};
use byteorder::{ReadBytesExt, WriteBytesExt};
use flate2::{Compression, GzBuilder, read::GzDecoder};
use config::Config;

pub struct SqliteStorageBackend {
	conn :Connection,
	ctr :u32,
}

/// Magic used to identify the mehlon application.
///
/// This magic was taken from hexdump -n 32 /dev/urandom output.
const MEHLON_SQLITE_APP_ID :i32 = 0x84eeae3cu32 as i32;

const USER_VERSION :u16 = 1;

/// We group multiple writes into transactions
/// as each transaction incurs a time penalty,
/// which added up, makes having one transaction
/// per write really slow.
const WRITES_PER_TRANSACTION :u32 = 50;

fn init_db(conn :&mut Connection) -> Result<(), StrErr> {
	set_app_id(conn, MEHLON_SQLITE_APP_ID)?;
	set_user_version(conn, USER_VERSION)?;
	conn.execute(
		"CREATE TABLE IF NOT EXISTS kvstore (
			kkey VARCHAR(16) PRIMARY KEY,
			content BLOB
		);",
		NO_PARAMS,
	)?;
	conn.execute(
		"CREATE TABLE IF NOT EXISTS chunks (
			x INTEGER,
			y INTEGER,
			z INTEGER,
			content BLOB,
			PRIMARY KEY(x, y, z)
		)",
		NO_PARAMS,
	)?;
	Ok(())
}

fn expect_user_ver(conn :&mut Connection) -> Result<(), StrErr> {
	let app_id = get_app_id(conn)?;
	let user_version = get_user_version(conn)?;
	if app_id != MEHLON_SQLITE_APP_ID {
		Err(format!("expected app id {} but was {}",
			MEHLON_SQLITE_APP_ID, app_id))?;
	}
	if user_version != USER_VERSION {
		Err(format!("expected user_version {} but was {}",
			USER_VERSION, user_version))?;
	}
	Ok(())
}

fn get_user_version(conn :&mut Connection) -> Result<u16, StrErr> {
	let r = conn.query_row("PRAGMA user_version;", NO_PARAMS, |v| v.get(0))?;
	Ok(r)
}
fn set_user_version(conn :&mut Connection, version :u16) -> Result<(), StrErr> {
	// Apparently sqlite wants you to be exposed to bobby tables shit
	// because they don't allow you to use ? or other methods to avoid
	// string formatting :/.
	conn.execute(&format!("PRAGMA user_version = {};", version), NO_PARAMS)?;
	Ok(())
}
fn get_app_id(conn :&mut Connection) -> Result<i32, StrErr> {
	let r = conn.query_row("PRAGMA application_id;", NO_PARAMS, |v| v.get(0))?;
	Ok(r)
}
fn set_app_id(conn :&mut Connection, id :i32) -> Result<(), StrErr> {
	// Apparently sqlite wants you to be exposed to bobby tables shit
	// because they don't allow you to use ? or other methods to avoid
	// string formatting :/.
	conn.execute(&format!("PRAGMA application_id = {};", id), NO_PARAMS)?;
	Ok(())
}

impl SqliteStorageBackend {
	pub fn from_conn(mut conn :Connection, freshly_created :bool) -> Result<Self, StrErr> {
		if freshly_created {
			init_db(&mut conn)?;
		} else {
			expect_user_ver(&mut conn)?;
		}

		Ok(Self {
			conn,
			ctr : 0,
		})
	}
	pub fn open_or_create(path :impl AsRef<Path> + Clone) -> Result<Self, StrErr> {
		// SQLite doesn't tell us whether a newly opened sqlite file has been
		// existing on disk previously, or just been created.
		// Thus, we need to do two calls: first one which doesn't auto-create,
		// then one which does.

		let conn = Connection::open_with_flags(path.clone(), OpenFlags::SQLITE_OPEN_READ_WRITE);
		match conn {
			Ok(conn) => Ok(Self::from_conn(conn, false)?),
			Err(rusqlite::Error::SqliteFailure(e, _))
					if e.code == libsqlite3_sys::ErrorCode::CannotOpen => {
				println!("cannot open");
				let conn = Connection::open(path)?;
				Ok(Self::from_conn(conn, true)?)
			},
			Err(v) => Err(v)?,
		}
	}
}

fn mapblock_to_number(b :MapBlock) -> u8 {
	use MapBlock::*;
	match b {
		Air => 0,
		Water => 1,
		Sand => 2,
		Ground => 3,
		Wood => 4,
		Stone => 5,
		Leaves => 6,
		Tree => 7,
		Cactus => 8,
		Coal => 9,
	}
}

fn number_to_mapblock(b :u8) -> Option<MapBlock> {
	use MapBlock::*;
	Some(match b {
		0 => Air,
		1 => Water,
		2 => Sand,
		3 => Ground,
		4 => Wood,
		5 => Stone,
		6 => Leaves,
		7 => Tree,
		8 => Cactus,
		9 => Coal,
		_ => return None,
	})
}

fn serialize_mapchunk_data(data :&MapChunkData) -> Vec<u8> {
	let mut blocks = Vec::new();
	for b in data.0.iter() {
		blocks.write_u8(mapblock_to_number(*b)).unwrap();
	}
	let rdr :&[u8] = &blocks;
	let mut gz_enc = GzBuilder::new().read(rdr, Compression::fast());
	let mut r = Vec::<u8>::new();

	// Version
	r.write_u8(0).unwrap();
	io::copy(&mut gz_enc, &mut r).unwrap();
	r
}

fn deserialize_mapchunk_data(data :&[u8]) -> Result<MapChunkData, StrErr> {
	let mut rdr = data;
	let version = rdr.read_u8()?;
	if version != 0 {
		// The version is too recent
		Err(format!("Unsupported map chunk version {}", version))?;
	}
	let mut gz_dec = GzDecoder::new(rdr);
	let mut buffer = Vec::<u8>::new();
	io::copy(&mut gz_dec, &mut buffer)?;
	let mut rdr :&[u8] = &buffer;
	let mut r = MapChunkData::fully_air();
	for v in r.0.iter_mut() {
		let n = rdr.read_u8()?;
		*v = number_to_mapblock(n).ok_or("invalid block number")?;
	}
	Ok(r)
}

impl StorageBackend for SqliteStorageBackend {
	fn store_chunk(&mut self, pos :Vector3<isize>,
			data :&MapChunkData) -> Result<(), StrErr> {
		let pos = pos / CHUNKSIZE;
		let data = serialize_mapchunk_data(&data);
		if self.ctr == 0 {
			self.ctr = WRITES_PER_TRANSACTION;
			if !self.conn.is_autocommit() {
				let mut stmt = self.conn.prepare_cached("COMMIT;")?;
				stmt.execute(NO_PARAMS)?;
			}
		} else {
			self.ctr -= 1;
		}
		if self.conn.is_autocommit() {
			let mut stmt = self.conn.prepare_cached("BEGIN;")?;
			stmt.execute(NO_PARAMS)?;
		}
		let mut stmt = self.conn.prepare_cached("INSERT OR REPLACE INTO chunks (x, y, z, content) \
			VALUES (?, ?, ?, ?);")?;
		stmt.execute(&[&pos.x as &dyn ToSql, &pos.y, &pos.z, &data])?;
		Ok(())
	}
	fn tick(&mut self) -> Result<(), StrErr> {
		if !self.conn.is_autocommit() {
			self.ctr = WRITES_PER_TRANSACTION;
			let mut stmt = self.conn.prepare_cached("COMMIT;")?;
			stmt.execute(NO_PARAMS)?;
		}
		Ok(())
	}
	fn load_chunk(&mut self, pos :Vector3<isize>) -> Result<Option<MapChunkData>, StrErr> {
		let pos = pos / CHUNKSIZE;
		let mut stmt = self.conn.prepare_cached("SELECT content FROM chunks WHERE x=? AND y=? AND z=?")?;
		let data :Option<Vec<u8>> = stmt.query_row(
			&[&pos.x, &pos.y, &pos.z],
			|row| row.get(0)
		).optional()?;
		if let Some(data) = data {
			let chunk = deserialize_mapchunk_data(&data)?;
			Ok(Some(chunk))
		} else {
			Ok(None)
		}
	}
	fn get_global_kv(&mut self, key :&str) -> Result<Option<Vec<u8>>, StrErr> {
		let mut stmt = self.conn.prepare_cached("SELECT content FROM kvstore WHERE kkey=?")?;
		let data :Option<Vec<u8>> = stmt.query_row(
			&[&key],
			|row| row.get(0)
		).optional()?;
		Ok(data)
	}
	fn set_global_kv(&mut self, key :&str, content :&[u8]) -> Result<(), StrErr> {
		let mut stmt = self.conn.prepare_cached("INSERT OR REPLACE INTO kvstore (kkey, content) \
			VALUES (?, ?);")?;
		stmt.execute(&[&key as &dyn ToSql, &content])?;
		Ok(())
	}
}

pub struct NullStorageBackend;

impl StorageBackend for NullStorageBackend {
	fn store_chunk(&mut self, _pos :Vector3<isize>,
			_data :&MapChunkData) -> Result<(), StrErr> {
		Ok(())
	}
	fn tick(&mut self) -> Result<(), StrErr> {
		Ok(())
	}
	fn load_chunk(&mut self, _pos :Vector3<isize>) -> Result<Option<MapChunkData>, StrErr> {
		Ok(None)
	}
	fn get_global_kv(&mut self, _key :&str) -> Result<Option<Vec<u8>>, StrErr> {
		Ok(None)
	}
	fn set_global_kv(&mut self, _key :&str, _content :&[u8]) -> Result<(), StrErr> {
		Ok(())
	}
}

pub type DynStorageBackend = Box<dyn StorageBackend + Send>;

fn sqlite_backend_from_config(config :&Config) -> Option<DynStorageBackend> {
	let p = config.map_storage_path.as_ref()?;
	let sqlite_backend = match SqliteStorageBackend::open_or_create(p) {
		Ok(b) => b,
		Err(e) => {
			println!("Error while opening database: {:?}", e);
			return None;
		},
	};
	Some(Box::new(sqlite_backend))
}

pub fn storage_backend_from_config(config :&Config) -> DynStorageBackend {
	sqlite_backend_from_config(config).unwrap_or_else(|| {
		Box::new(NullStorageBackend)
	})
}

pub trait StorageBackend {
	fn store_chunk(&mut self, pos :Vector3<isize>,
			data :&MapChunkData) -> Result<(), StrErr>;
	fn tick(&mut self) -> Result<(), StrErr>;
	fn load_chunk(&mut self, pos :Vector3<isize>) -> Result<Option<MapChunkData>, StrErr>;
	fn get_global_kv(&mut self, key :&str) -> Result<Option<Vec<u8>>, StrErr>;
	fn set_global_kv(&mut self, key :&str, content :&[u8]) -> Result<(), StrErr>;
}
