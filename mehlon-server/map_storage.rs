use rusqlite::{Connection, NO_PARAMS, OptionalExtension, OpenFlags};
use map::{MapChunkData, MapBlock, CHUNKSIZE};
use StrErr;
use nalgebra::Vector3;
use std::path::Path;
use byteorder::{ReadBytesExt, WriteBytesExt};

pub struct SqliteStorageBackend {
	conn :Connection,
}

/// Magic used to identify the mehlon application.
///
/// This magic was taken from hexdump -n 32 /dev/urandom output.
const MEHLON_SQLITE_APP_ID :i32 = 0x84eeae3cu32 as i32;

const USER_VERSION :u16 = 1;

fn init_db(conn :&mut Connection) -> Result<(), StrErr> {
	set_app_id(conn, MEHLON_SQLITE_APP_ID)?;
	set_user_version(conn, USER_VERSION)?;
	conn.execute(
		"CREATE TABLE IF NOT EXISTS kvstore (
			key VARCHAR(16) PRIMARY KEY,
			content BLOB,
		)",
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
	conn.execute("PRAGMA user_version = ?;", &[&version])?;
	Ok(())
}
fn get_app_id(conn :&mut Connection) -> Result<i32, StrErr> {
	let r = conn.query_row("PRAGMA application_id;", NO_PARAMS, |v| v.get(0))?;
	Ok(r)
}
fn set_app_id(conn :&mut Connection, id :i32) -> Result<(), StrErr> {
	conn.execute("PRAGMA application_id = ?;", &[&id])?;
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
	let mut r = Vec::new();
	for b in data.0.iter() {
		r.write_u8(mapblock_to_number(*b));
	}
	r
}

fn deserialize_mapchunk_data(data :&[u8]) -> Result<MapChunkData, StrErr> {
	let mut rdr = data;
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
		// TODO prepare this statement
		self.conn.execute_named("UPDATE OR INSERT INTO chunks (x, y, z, content) \
			VALUES (:x, :y, :z, :content)",
			&[(":x", &pos.x), (":y", &pos.y), (":z", &pos.z), (":content", &data)])?;
		Ok(())
	}
	fn load_chunk(&mut self, pos :Vector3<isize>) -> Result<Option<MapChunkData>, StrErr> {
		let pos = pos / CHUNKSIZE;
		// TODO prepare this statement
		let data :Option<Vec<u8>> = self.conn.query_row("SELECT content FROM chunks WHERE x=?,y=?,z=?",
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
}

pub trait StorageBackend {
	fn store_chunk(&mut self, pos :Vector3<isize>,
			data :&MapChunkData) -> Result<(), StrErr>;
	fn load_chunk(&mut self, pos :Vector3<isize>) -> Result<Option<MapChunkData>, StrErr>;
}