//	vim:set ts=4 sw=4 sts=0 fileencoding=utf-8:
//	----------------------------------------------------------------------------
/*
	@file		main.rs
	@author		zuntan
*/
//	----------------------------------------------------------------------------

use std::env;
use std::io::prelude::*;
use std::io::{ BufRead, BufReader, Write };
use std::net;
use std::net::TcpStream;
use std::fmt;
use std::borrow::Cow;
use std::thread;
use std::time::Duration;
use std::sync::mpsc;
use std::collections::HashMap;
use std::str::FromStr;
use std::cell::{ RefCell, Ref, RefMut };

extern crate getopts;
extern crate shell_words;
extern crate wildmatch;

#[macro_use]
extern crate lazy_static;
extern crate regex;

use rustyline::error::ReadlineError;
use rustyline;
use rustyline::{ completion::Completer, Context };

struct ExecOk
{
    flds:       Vec<(String,String)>
,   bin:        Option<Vec<u8>>
}

struct ExecErr
{
    err_code:   i32
,   cmd_index:  i32
,   cur_cmd:    Option<String>
,   msg_text:   Option<String>
}

impl fmt::Display for ExecErr
{
    fn fmt( &self, f: &mut fmt::Formatter<'_> ) -> fmt::Result
    {
        match &self.msg_text
        {
            Some( x )   => write!( f, "code:{} msg:{}", self.err_code, x )
        ,   None        => write!( f, "code:{}", self.err_code )
        }
    }
}

type ExecResult = Result<ExecOk, ExecErr>;

#[derive(Debug, Clone)]
struct ListEntry
{
    name:       String
,   name_type:  String
,   flds:       Vec<(String,String)>
}

impl ListEntry
{
    fn new() -> ListEntry
    {
        ListEntry{ name: String::new(), name_type: String::new(), flds: Vec::<(String,String)>::new() }
    }
}

struct Mpdsh
{
    worker_handle:  thread::JoinHandle<()>
,   htx:            mpsc::Sender<String>
,   wrx:            mpsc::Receiver<ExecResult>
,   version:        String
,   curdir:         String
,   args:           Vec<String>
,   args_opt:       Vec<String>
}

impl Mpdsh
{
    fn new( stream: TcpStream, opt_protolog: bool ) -> Result< Self, () >
    {
        let mut reader = BufReader::new( &stream );
        let mut buf = String::new();

        reader.read_line( &mut buf ).expect( "failed to read from socket" );

        println!( "connected {}", &buf );

        if !buf.starts_with("OK MPD ")
        {
            return Err(());
        }

        let version = String::from( buf[7..].trim() );

        let ( htx, hrx ) : ( mpsc::Sender<String>,      mpsc::Receiver<String> )        = mpsc::channel();
        let ( wtx, wrx ) : ( mpsc::Sender<ExecResult>,  mpsc::Receiver<ExecResult> )    = mpsc::channel();

        let worker_handle  = thread::spawn( move ||
            {
                Self::worker( stream, hrx, wtx, opt_protolog )
            }
        );

        Ok( Mpdsh
            {
                worker_handle:  worker_handle
            ,   htx:            htx
            ,   wrx:            wrx
            ,   version:        version
            ,   curdir:         String::from( "/" )
            ,   args:           Vec::new()
            ,   args_opt:       Vec::new()
            }
        )
    }

    fn exec_command( &self, cmd: &str ) -> ExecResult
    {
        match self.htx.send( String::from( cmd ) )
        {
			Ok(_)	=> {

				match self.wrx.recv()
		        {
					Ok(x)	=> {
						return x;
					}
				,	Err(_)	=> {}
				}

			}
		,	Err(_)	=> {}
		};

		Err(
			ExecErr
			{
			    err_code:   -2
			,   cmd_index:  0
			,   cur_cmd:    None
			,   msg_text:   None
			}
		)
    }

    fn worker( mut stream: TcpStream, hrx : mpsc::Receiver<String>, wtx : mpsc::Sender<ExecResult>, protolog : bool )
    {
		let mut io_err : Option< std::io::Error > = None;

        let time_out = Duration::from_secs( 10 );

        'outer: loop
        {
            let recv = hrx.recv_timeout( time_out );
            let ( cmd, ret ) = match recv
            {
                Ok( x ) => { ( Cow::Owned( x )          , true ) }
            ,   Err(_)  => { ( Cow::Borrowed( "ping" )  , false ) }
            };

			if cmd == "quit"
			{
				wtx.send(
					Ok(
						ExecOk
						{
            				flds: 	Vec::<(String, String)>::new()
            			,	bin: 	Option::<Vec<u8>>::None
						}
					)
				);

                break 'outer;
			}

			if let Err(x) = stream
                .write( cmd.as_bytes() )
                .and_then(|_| stream.write( &[0x0a] ) )
                .and_then(|_| stream.flush() )
            {
				io_err = Some( x );
                break 'outer;
			}

            if protolog && ret
            {
                eprintln!( "> {}", cmd );
            }

            let mut reader = BufReader::new( &stream );

            let mut buf = String::new();

            let mut flds            = Vec::<(String, String)>::new();
            let mut bin             = Option::<Vec<u8>>::None;
            let mut err_code:   i32 = 0;
            let mut cmd_index:  i32 = 0;
            let mut cur_cmd         = Option::<String>::None;
            let mut msg_text        = Option::<String>::None;

            let send;

            loop
            {
                buf.clear();

                match reader.read_line( &mut buf )
                {
					Ok(x) =>
					{
						if x == 0
						{
							break 'outer;
						}
					}
				,	Err(x) =>
					{
						io_err = Some( x );
						break 'outer;
					}
				};

                if protolog && ret
                {
                    eprint!("< {}", buf );
                }

                if buf == "OK\n"
                {
                    send = Ok( ExecOk { flds, bin } );
                    break;
                }
                else if buf.starts_with( "ACK [" )
                {
                    lazy_static! {
                        static ref RE: regex::Regex =
                            regex::Regex::new( r"^ACK\s*\[(\d+)@(\d+)\]\s+\{([^}]*)\}\s*(.*)\n" ).unwrap();
                    }

                    if let Some( x ) = RE.captures( &buf )
                    {
                        err_code    = x[1].parse().unwrap();
                        cmd_index   = x[2].parse().unwrap();
                        cur_cmd     = Some( String::from( &x[3] ) );
                        msg_text    = Some( String::from( &x[4] ) );
                    };

                    send = Err( ExecErr { err_code, cmd_index, cur_cmd, msg_text } );
                    break;
                }
                else
                {
                    lazy_static! {
                        static ref RE: regex::Regex =
                            regex::Regex::new( r"^([^:]*):\s*(.*)\n" ).unwrap();
                    }

                    if let Some( x ) = RE.captures( &buf )
                    {
                        if &x[1] == "binary"
                        {
                            let binlen = x[2].parse().unwrap();
                            let mut buf = Vec::<u8>::with_capacity(binlen);
                            unsafe
                            {
                                buf.set_len( binlen );
                            }

                            match reader.read( &mut buf )
                            {
								Ok(_) =>
								{
		                            bin = Some( buf )
								}
							,	Err(x) =>
								{
									io_err = Some( x );
		                            break 'outer;
								}
							}
                        }
                        else
                        {
                            flds.push(
                                (
                                    String::from( x[1].trim() )
                                ,   String::from( x[2].trim() )
                                )
                            );
                        }
                    }
                }
            }

            if ret
            {
                wtx.send( send );
            }
        }

        if io_err.is_some()
        {
			eprintln!( "" );
			eprintln!( "{:?}", io_err.as_ref().unwrap() );
			std::process::exit(1);
		}

		else if let Err(x) = stream.shutdown( std::net::Shutdown::Both )
		{
			eprintln!( "" );
			eprintln!( "{:?}", x );
		}
    }

    fn prompt( &self ) -> String
    {
        format!( "mpdsh:{}> ", &self.curdir )
    }

    fn setup_args( &mut self, args : Vec<String> )
    {
        self.args.clear();
        self.args_opt.clear();

        for x in args
        {
            if x.starts_with( "#" )
            {
                break;
            }

            if x.starts_with( "-" )
            {
                self.args_opt.push( x );
            }
            else
            {
                self.args.push( x );
            }
        }
    }

    fn cmdline( &mut self, args : Vec<String> ) -> bool
    {
        self.setup_args( args );

        if !self.args.is_empty()
        {
            match self.args[0].as_str()
            {
                "cd"                    => self.cmd_cd()
            ,   "ls"                    => self.cmd_ls()

            ,   "pl"        | "plist"   => self.cmd_pl()
            ,   "add"       | "a"       => self.cmd_ls()
            ,   "add_top"   | "at"      => self.cmd_ls()
            ,   "add_uri"               => self.cmd_with_args( "addid", 2 )
            ,   "del"                   => self.cmd_with_args( "delete", 1 )
            ,   "clr"                   => self.cmd_with_args( "clear", 0 )
            ,   "move"                  => self.cmd_with_args( "move", 2 )

            ,   "play"      | "p"       => self.cmd_with_args( "play", 1 )
            ,   "stop"      | "s"       => self.cmd_with_args( "stop", 0 )
            ,   "pause"     | "u"       => self.cmd_with_args( "pause 1", 0 )
            ,   "resume"    | "e"       => self.cmd_with_args( "pause 0", 0 )
            ,   "prev"      | "r"       => self.cmd_with_args( "previous", 0 )
            ,   "next"      | "n"       => self.cmd_with_args( "next", 0 )

            ,   "random"                => self.cmd_switch( "random" )
            ,   "repeat"                => self.cmd_switch( "repeat" )
            ,   "single"                => self.cmd_switch( "single" )
            ,   "volume"    | "v"       => self.cmd_switch( "setvol" )

            ,   "status"    | "st"      => self.cmd_status()

            ,   "update"                => self.cmd_with_args( "update", 1 )
            ,   "cmd"                   => self.cmd_cmd()

            ,   "help"      | "h"       => self.cmd_help()
            ,   "quit"      | "q"       => { self.cmd_quit(); return true; }
            ,   _                       => self.cmd_unknown()
            }
        }

        false
    }

    fn has_opt( &self, opt : &str ) -> bool
    {
        self.args_opt.iter().find( |&x| x == opt ) != None
    }

    fn cmdline_hint( &mut self, args : Vec<String> ) -> ( Vec<String>, usize )
    {
        self.setup_args( args );

        if self.args.is_empty()
        {
            return ( Self::cmdlist(), 0 )
        }
        else
        {
            match self.args[0].as_str()
            {
                "cd"    => { return self.hint_entry( false ); }
            ,   "ls" | "add" | "a"
                        => { return self.hint_entry( true ); }
            ,   "help"  => {
                    if self.args.len() == 1
                    {
                        return ( Self::cmdlist(), 0 )
                    }
                }
            ,   _       => {}
            }
        }

        ( Vec::<String>::new(), 0 )
    }

    fn get_arge1_path( &self ) -> String
    {
        let dir;

        if self.args[1].starts_with( "/" )
        {
            dir = String::from( format!( "{}", &self.args[1] ) );
        }
        else
        {
            dir = String::from( format!( "{}/{}", self.curdir, &self.args[1] ) )
        }

        Self::make_canonical_path( &dir )
    }

    fn make_canonical_path( path : &str ) -> String
    {
        let mut parts : Vec<&str> = Vec::new();

        for part in path.trim_start_matches('/').split('/')
        {
            match part
            {
                "." | ""    => {} // nop
            ,   ".."        => { if !parts.is_empty() { parts.pop(); } }
            ,   _           => { parts.push( part ); }
            }
        }

        parts.join( "/" )
    }

    fn make_parent_path( path : &str ) -> ( String, String )
    {
        let parts : Vec<&str> = path.trim_start_matches('/').split('/').collect();
        let mut p_dir  = String::new();
        let mut c_name = String::new();

        if parts.len() >= 1
        {
            c_name = String::from( parts[ parts.len() - 1 ] )
        }

        if parts.len() > 1
        {
            p_dir = parts[.. parts.len() - 1 ].join( "/" )
        }

        ( p_dir, c_name )
    }

    fn split_listfiles( flds : Vec< ( String, String ) > ) -> Vec< ListEntry >
    {
        let mut ret = Vec::< ListEntry >::new();

        let mut le = ListEntry::new();

        for ( k, v ) in flds.iter().rev()
        {
            if k == "directory" || k == "file" || k == "playlist"
            {
                le.name = v.to_string();
                le.name_type = k.to_string();
                le.flds.reverse();
                ret.push( le );
                le = ListEntry::new();
            }
            else
            {
                le.flds.push( ( k.to_string(), v.to_string() ) );
            }
        }

        ret.reverse();
        ret
    }

    fn format_duration( sec_str: &str ) -> Result< String, () >
    {
        if let Ok( x ) = f32::from_str( sec_str.trim() )
        {
            let x = x as i32;
            let sec = x % 60;
            let min =  ( x - sec ) / 60;
            let hour = ( x - sec - min * 60 ) / 60 * 60;

            return Ok( String::from( format!( "{:02}:{:02}:{:02}", hour, min, sec ) ) )
        }

        Err( () )
    }

    fn cmd_cd( &mut self )
    {
        if self.args.len() >= 2
        {
            let dir = self.get_arge1_path();
            let dir = Self::make_canonical_path( &dir );

            let ( p_dir, c_name ) = Self::make_parent_path( &dir );

            let command = &Self::make_command_1( "listfiles", &p_dir );

            match self.exec_command( &command )
            {
                Ok( x ) =>
                {
                    if c_name == "" || x.flds.iter().find(|&x| x.0 == "directory" && x.1 == c_name ) != None
                    {
                        self.curdir = String::from( "/" ) + &dir;
                    }
                    else
                    {
                        eprint!( "No such directory" );
                    }
                }
            ,   Err( x ) => self.show_error( &x )
            }
        }
    }

    fn cmd_pl( &mut self )
    {
        let mut songid_cur  : Option< String > = None;
        let mut songid_next : Option< String > = None;

        match self.exec_command( "status" )
        {
            Ok( x ) =>
            {
                for( k, v ) in x.flds
                {
                    if k == "songid"
                    {
                        songid_cur = Some( v );
                    }
                    else if k == "nextsongid"
                    {
                        songid_next = Some( v );
                    }
                }
            }
        ,   Err(_) => {}
        };

        match self.exec_command( "playlistinfo" )
        {
            Ok(x) =>
            {
                let mut pos = 0;

                for entry in Self::split_listfiles( x.flds )
                {
                    let mut flg = "";

                    for ( k, v ) in &entry.flds
                    {
                        if k == "Id"
                        {
                            if songid_cur.is_some() && songid_cur.as_ref().unwrap() == v
                            {
                                flg = "=>"
                            }
                            else if songid_next.is_some() && songid_next.as_ref().unwrap() == v
                            {
                                flg = "."
                            }

                            break;
                        }
                    }

                    if self.has_opt( "-l" ) && !entry.flds.is_empty() || pos == 0
                    {
                        println!( "" );
                    }

                    println!( "{:2}{:4}| {:9}: {}", flg, pos, entry.name_type, entry.name );

                    if self.has_opt( "-l" )
                    {
                        for ( k, mut v ) in entry.flds
                        {
                            if k == "duration"
                            {
                                if let Ok(x) = Self::format_duration( &v )
                                {
                                    v = x;
                                };
                            }

                            println!( "      | {:9}: {}", k, v );
                        }
                    }

                    pos += 1;
                }

                if pos == 0
                {
                    println!( "No files ..." );
                }
                else
                {
                    println!( "" );
                }
            }
        ,   Err(x) => self.show_error( &x )
        }
    }

    fn hint_playlist( &mut self ) -> Vec<String>
    {
        match self.exec_command( "playlistinfo" )
        {
            Ok( x ) =>
            {
                let mut ret = Vec::<String>::new();

                for ( _, nm ) in x.flds.iter().filter(|&x| x.0 == "file" )
                {
                    let mut val = nm.clone();

                    if val.contains( ' ' )
                    {
                        val = String::from( "\"" ) + &val + "\"";
                    }

                    ret.push( val );
                }

                return ret;
            }
        ,   Err(_x) => {}
        }

        return Vec::<String>::new();
    }

    fn cmd_ls( &mut self )
    {
        let cmd_add     = self.args[0] == "add"         || self.args[0] == "a"  ;
        let cmd_add_top = self.args[0] == "add_top"     || self.args[0] == "at" ;

        let mut dir;
        let mut wmatch = None;

        if self.args.len() >= 2
        {
            dir = self.get_arge1_path();
            dir = Self::make_canonical_path( &dir );

            let ( p_dir, c_name ) = Self::make_parent_path( &dir );

            if c_name.contains( '*' ) || c_name.contains( '?' )
            {
                dir = p_dir;
                wmatch = Some( c_name );
            }
        }
        else
        {
            dir = String::from( &self.curdir );
            dir = Self::make_canonical_path( &dir );
        };

        let cmd = if self.has_opt( "-l" ) || cmd_add || cmd_add_top
        {
            "lsinfo"
        }
        else
        {
            "listfiles"
        };

        match self.exec_command( &Self::make_command_1( cmd, &dir ) )
        {
            Ok( x ) =>
            {
                if cmd == "lsinfo"
                {
                    let mut tmp = Self::split_listfiles( x.flds );

                    if wmatch.is_some()
                    {
                        let wmatch_ptn = wildmatch::WildMatch::new( &wmatch.unwrap() );

                        let mut tmp2 = Vec::< ListEntry >::new();

                        for entry in tmp
                        {
                            let ( _p_dir, c_name ) = Self::make_parent_path( &entry.name );

                            if wmatch_ptn.is_match( &c_name )
                            {
                                tmp2.push( entry );
                            }
                        }

                        tmp = tmp2;
                    }

                    let mut pos = 0;

                    for entry in tmp
                    {
                        if cmd_add || cmd_add_top
                        {
                            if entry.name_type == "file" || entry.name_type == "playlist"
                            {
                                let cmd = if cmd_add_top
                                {
                                    String::from( &Self::make_command_2( "addid", &entry.name, &pos.to_string() ) )
                                }
                                else
                                {
                                    String::from( &Self::make_command_1( "add", &entry.name ) )
                                };

                                match self.exec_command( &cmd )
                                {
                                    Ok(_) =>
                                    {
                                        println!( " A {:9}: {}", entry.name_type, entry.name );
                                        pos += 1;
                                    }
                                ,   Err(x) =>
                                    {
                                        self.show_error( &x );
                                        break
                                    }
                                }
                            }
                        }
                        else
                        {
                            if !entry.flds.is_empty() || pos == 0
                            {
                                println!( "" );
                            }

                            println!( "{:12}: {}", entry.name_type, entry.name );

                            for ( k, mut v ) in entry.flds
                            {
                                if k == "duration"
                                {
                                    if let Ok(x) = Self::format_duration( &v )
                                    {
                                        v = x;
                                    };
                                }

                                println!( " | {:9}: {}", k, v );
                            }

                            pos += 1;
                        }
                    }

                    if ( cmd_add || cmd_add_top ) && pos == 0
                    {
                        println!( "No files added..." );
                    }
                    else if pos != 0
                    {
                        println!( "" );
                    }
                }
                else
                {
                    let mut tmp : Vec<&(String,String)> = x.flds.iter().filter( |&x| x.0 == "directory" || x.0 == "file" ).collect();

                    if wmatch.is_some()
                    {
                        let wmatch_ptn = wildmatch::WildMatch::new( &wmatch.unwrap() );

                        tmp = tmp.iter().map( |x| *x ).filter( |&x| wmatch_ptn.is_match( &x.1 ) ).collect();
                    }

                    for ( tp, nm ) in tmp
                    {
                        if !cmd_add
                        {
                            println!( "{:12}: {}", tp, nm );
                        }
                    }
                }
            }
        ,   Err( x ) => self.show_error( &x )
        }
    }

    fn hint_entry( &mut self, with_file : bool ) -> ( Vec<String>, usize )
    {
        let mut dir;
        let mut wmatch = None;

        if self.args.len() >= 2
        {
            dir = self.get_arge1_path();
            dir = Self::make_canonical_path( &dir );

            let ( p_dir, c_name ) = Self::make_parent_path( &dir );

            if c_name.contains( '*' ) || c_name.contains( '?' )
            {
                dir = p_dir;
                wmatch = Some( c_name );
            }
        }
        else
        {
            dir = String::from( &self.curdir );
            dir = Self::make_canonical_path( &dir );
        };

        match self.exec_command( &Self::make_command_1( "listfiles", &dir ) )
        {
            Ok( x ) =>
            {
                let mut tmp : Vec<&(String,String)> = x.flds.iter().filter( |&x| x.0 == "directory" || with_file && x.0 == "file" ).collect();

                if wmatch.is_some()
                {
                    let wmatch_ptn = wildmatch::WildMatch::new( &wmatch.unwrap() );

                    tmp = tmp.iter().map( |x| *x ).filter( |&x| wmatch_ptn.is_match( &x.1 ) ).collect();
                }

                let mut ret = Vec::<String>::new();

                for ( _, nm ) in tmp
                {
                    let mut val = nm.clone();

                    if val.contains( ' ' )
                    {
                        val = String::from( "\"" ) + &val + "\"";
                    }

                    ret.push( val );
                }

                return ( ret, 0 );
            }
        ,   Err( x ) => {
                if x.err_code == 50 /* No such object */
                {
                    let ( p_dir, c_name ) = Self::make_parent_path( &dir );

                    match self.exec_command( &Self::make_command_1( "listfiles", &p_dir ) )
                    {
                        Ok( x ) =>
                            {
                                let tmp : Vec<&(String,String)> = x.flds.iter().filter( |&x| x.0 == "directory" || with_file && x.0 == "file" ).collect();

                                let mut ret = Vec::<String>::new();

                                for ( _, nm ) in tmp
                                {
                                    if nm.starts_with( &c_name )
                                    {
                                        let mut val = nm.clone();

                                        if val.contains( ' ' )
                                        {
                                            val = String::from( "\"" ) + &val + "\"";
                                        }

                                        ret.push( val );
                                    }
                                }

                                return ( ret, c_name.len() );
                            }
                        ,   Err(_) => {}
                    }
                }
            }
        }

        return ( Vec::<String>::new(), 0 )
    }

    fn cmd_with_args( &self, cmd1 : &str, num_args : usize )
    {
        let mut cmd = String::new();

        cmd.push_str( cmd1 );

        let mut i = 0;

        for x in self.args.iter().skip(1)
        {
            if i >= num_args
            {
                break;
            }

            i += 1;

            cmd.push_str( " " );
            cmd.push_str( &Self::quote_arges( x ) );
        }

        match self.exec_command( &cmd )
        {
            Ok(_) =>
            {
                println!( "OK." );
            }
        ,   Err(x) => self.show_error( &x )
        }
    }

    fn cmd_switch( &self, cmd1 : &str )
    {
        if self.args.len() > 1
        {
            self.cmd_with_args( cmd1, 1 )
        }
        else
        {
            let key = match cmd1
            {
                "setvol" => { "volume" }
            ,   _        => { cmd1 }
            };

            match self.exec_command( "status" )
            {
                Ok( x ) =>
                {
                    for ( k, v ) in x.flds
                    {
                        if k == key
                        {
                            println!( "" );
                            println!( "{:>10}: {}", k, v );
                            println!( "" );
                            break;
                        }
                    }
                }
            ,   Err( x ) => self.show_error( &x )
            }
        }
    }

    fn cmd_status( &self )
    {
        match self.exec_command( "status" )
        {
            Ok( x ) =>
            {
                let mut st = HashMap::<String, String>::new();

                for ( k, v ) in x.flds
                {
                    st.insert( k, v );
                }

                let sp = String::new();

                println!( "" );
                println!( "{:>10}: {}", "State",    &st.get( "state"    ).unwrap_or( &sp ) );
                println!( "" );
                println!( "{:>10}: {}", "Volume",   &st.get( "volume"   ).unwrap_or( &sp ) );
                println!( "{:>10}: {}", "Repeat",   &st.get( "repeat"   ).unwrap_or( &sp ) );
                println!( "{:>10}: {}", "Repeat",   &st.get( "random"   ).unwrap_or( &sp ) );
                println!( "{:>10}: {}", "Single",   &st.get( "single"   ).unwrap_or( &sp ) );

                if st.contains_key( "songid" )
                {
                    let songid = st.get( "songid" ).unwrap();

                    match self.exec_command( "playlistinfo" )
                    {
                        Ok( x ) =>
                        {
                            let mut pls = HashMap::<String, HashMap::<String, String> >::new();
                            let mut ple = HashMap::<String, String>::new();

                            for ( k, v ) in x.flds.iter().rev()
                            {
                                ple.insert( String::from( k ), String::from( v ) );

                                if k == "file"
                                {
                                    if ple.contains_key( "Id" )
                                    {
                                        pls.insert( String::from( ple.get( "Id" ).unwrap() ), ple );
                                    }

                                    ple = HashMap::< String, String >::new();
                                }
                            }

                            if pls.contains_key( songid )
                            {
                                let ple = pls.get( songid ).unwrap();

                                println!( "" );
                                println!( "{:>10}: {}", "Now song", &ple.get( "file"    ).unwrap_or( &sp ) );
                                println!( "{:>10}: {}", "Artist",   &ple.get( "Artist"   ).unwrap_or( &sp ) );
                                println!( "{:>10}: {}", "Title",    &ple.get( "Title"    ).unwrap_or( &sp ) );
                                println!( "{:>10}: {}", "Album",    &ple.get( "Album"    ).unwrap_or( &sp ) );

                                if let Ok(x) = Self::format_duration( &st.get( "duration" ).unwrap_or( &sp ) )
                                {
                                    println!( "{:>10}: {}", "Duration", &x );
                                };

                                if let Ok(x) = Self::format_duration( &st.get( "elapsed" ).unwrap_or( &sp ) )
                                {
                                    println!( "{:>10}: {}", "Elapsed",  &x );
                                }

                                if st.contains_key( "audio" )
                                {
                                    let mut tmp = String::from( st.get( "audio" ).unwrap_or( &sp ) );

                                    if st.contains_key( "bitrate" )
                                    {
                                        tmp = String::from( format!( "{} (bitrate: {} Kbps)", tmp, st.get( "bitrate"    ).unwrap_or( &sp ) ) );
                                    }

                                    println!( "{:>10}: {}",     "Audio", tmp );
                                }

                                if st.contains_key( "nextsongid" )
                                {
                                    let songid = st.get( "nextsongid" ).unwrap();

                                    if pls.contains_key( songid )
                                    {
                                        let ple = pls.get( songid ).unwrap();

                                        println!( "" );
                                        println!( "{:>10}: {}", "Next song",    &ple.get( "file"    ).unwrap_or( &sp ) );
                                        println!( "{:>10}: {}",     "Artist",       &ple.get( "Artist"  ).unwrap_or( &sp ) );
                                        println!( "{:>10}: {}",     "Title",        &ple.get( "Title"   ).unwrap_or( &sp ) );
                                        println!( "{:>10}: {}",     "Album",        &ple.get( "Album"   ).unwrap_or( &sp ) );
                                    }
                                }
                            }
                        }
                    ,   Err( x ) => self.show_error( &x )
                    }
                }

                println!( "" );
            }
        ,   Err( x ) => self.show_error( &x )
        }
    }

    fn cmd_quit( &self )
    {
        match self.exec_command( "quit" )
        {
            Ok(_) => {}
        ,   Err(x) => self.show_error( &x )
        }
	}

    fn cmd_cmd( &self )
    {
        if self.args.len() < 2
        {
            println!( "Please specify MPD Command..." )
        }
        else
        {
            let mut cmd = String::new();

            cmd.push_str( &self.args[1] );

            for x in self.args.iter().skip(2)
            {
                cmd.push_str( " " );
                cmd.push_str( &Self::quote_arges( x ) );
            }

            match self.exec_command( &cmd )
            {
                Ok( x ) =>
                {
                    for ( k, v ) in x.flds
                    {
                        println!( "{}: {}", k, v );
                    }
                }
            ,   Err( x ) => self.show_error( &x )
            }
        }
    }

    fn cmdlist() -> Vec<String>
    {
        vec![
            "cd"
        ,   "ls"

        ,   "pl"
        ,   "add"
        ,   "add_top"
        ,   "add_uri"
        ,   "del"
        ,   "clr"
        ,   "move"

        ,   "play"
        ,   "stop"
        ,   "pause"
        ,   "resume"
        ,   "prev"
        ,   "next"

        ,   "random"
        ,   "repeat"
        ,   "single"
        ,   "volume"

        ,   "status"

        ,   "update"
        ,   "cmd"

        ,   "quit"
        ,   "help"
        ].iter().map( |&x| String::from( x ) ).collect()
    }

    fn cmd_help( &self )
    {
        if self.args.len() >= 2
        {
            let msg = match self.args[1].as_str()
            {
                "cd"                    => HELP_CD
            ,   "ls"                    => HELP_LS

            ,   "pl"        | "plist"   => HELP_PL
            ,   "add"       | "a"       => HELP_ADD
            ,   "add_top"   | "at"      => HELP_ADD_TOP
            ,   "add_uri"               => HELP_ADD_URI
            ,   "del"                   => HELP_DEL
            ,   "clr"                   => HELP_CLR
            ,   "move"                  => HELP_MOVE

            ,   "play"      | "p"       => HELP_PLAY
            ,   "stop"      | "s"       => HELP_STOP
            ,   "pause"     | "u"       => HELP_PAUSE
            ,   "resume"    | "e"       => HELP_RESUME

            ,   "prev"      | "r"       => HELP_PREV
            ,   "next"      | "n"       => HELP_NEXT

            ,   "random"                => HELP_RANDOM
            ,   "repeat"                => HELP_REPEAT
            ,   "single"                => HELP_SINGLE
            ,   "volume"    | "v"       => HELP_VOLUME

            ,   "status"                    => HELP_STATUS

            ,   "update"                => HELP_UPDATE
            ,   "cmd"                   => HELP_CMD

            ,   "help"      | "h"       => HELP_HELP
            ,   "quit"      | "q"       => HELP_QUIT
            ,   _                       => { "" }
            };

            if !msg.is_empty()
            {
                println!( "{}", &msg );
            }
        }

        println!( "" );
        print!( "help [ " );

        for x in Self::cmdlist()
        {
            print!( "{} ", &x );
        }

        println!( "]" );
        println!( "" );
    }

    fn cmd_unknown( &self )
    {
        println!( "unknown.. (use help command)" )
    }

    fn show_error( &self, err : &ExecErr )
    {
        println!( "error.. ({})", err )
    }

    fn quote_arges( arg: &str ) -> String
    {
        let mut arg = String::from( arg.replace('\\', r"\\").replace('"', r#"\""#) );

        if arg.contains( ' ' )
        {
            arg = String::from( "\"" ) + &arg + "\""
        }

        arg
    }

    fn make_command_1( cmd: &str, arg1: &str ) -> String
    {
        let mut ret = String::from( cmd );
        ret.push_str( " " );
        ret.push_str( &Self::quote_arges( arg1 ) );
        ret
    }

    fn make_command_2( cmd: &str, arg1: &str, arg2: &str ) -> String
    {
        let mut ret = String::from( cmd );
        ret.push_str( " " );
        ret.push_str( &Self::quote_arges( arg1 ) );
        ret.push_str( " " );
        ret.push_str( &Self::quote_arges( arg2 ) );
        ret
    }
}

#[derive( rustyline_derive::Helper, rustyline_derive::Validator, rustyline_derive::Highlighter, rustyline_derive::Hinter)]
struct RlHelper
{
    rc_mpdsh : RefCell< Mpdsh >
}

impl RlHelper
{
    fn borrow( &self ) -> Ref< Mpdsh >
    {
        self.rc_mpdsh.borrow()
    }

    fn borrow_mut( &self ) -> RefMut< Mpdsh >
    {
        self.rc_mpdsh.borrow_mut()
    }
}

impl Completer for RlHelper
{
    type Candidate = String;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> Result<( usize, Vec< String > ), ReadlineError >
    {
        match shell_words::split( &line )
        {
            Ok(args) =>
            {
                let ( entry, posd ) = self.borrow_mut().cmdline_hint( args );

                return Ok( ( pos - posd, entry ) );
            }
        ,   Err(_) => { /* nop */ }
        }

        Ok( ( 0, Vec::< String >::new() ) )         /* nop */
    }
}

fn usage( prog: &str, opts: getopts::Options )
{
    println!( "{}", opts.usage( &format!("Usage: {} [options]", prog ) ) );
}

const EX_USAGE: i32 = 64;

const PKG_NAME:     &'static str = env!("CARGO_PKG_NAME");
const PKG_VERSION:  &'static str = env!("CARGO_PKG_VERSION");
const PKG_AUTHORS:  &'static str = env!("CARGO_PKG_AUTHORS");

fn parse_opt() -> ( String, String, bool )
{
    let args: Vec<String> = env::args().collect();

    let arg_prog = args[0].clone();

    let mut opts = getopts::Options::new();

    opts.optopt( "h", "host", "MPD host address", "localhost" );
    opts.optopt( "p", "port", "MPD port number ", "6600" );
    opts.optflag( "d", "protolog", "Output protocol log to stderr." );
    opts.optflag( "v", "version", "Print version info and exit." );
    opts.optflag( "", "help", "Print this help menu." );

    let opt_matches = match opts.parse( &args[1..] )
    {
        Ok(m) => { m }
        Err(f) =>
        {
            println!( "{}", f.to_string() );
            usage( PKG_NAME, opts );
            std::process::exit( EX_USAGE );
        }
    };

    if opt_matches.opt_present( "help" )
    {
        usage( PKG_NAME, opts );
        std::process::exit( EX_USAGE );
    }

    if opt_matches.opt_present( "version" )
    {
        println!( "{} {}", PKG_NAME, PKG_VERSION );
        std::process::exit( EX_USAGE );
    }

    let opt_host = match opt_matches.opt_str( "host" )
    {
        Some(x) => { x }
    ,   None    => { "localhost".to_string() }
    };

    let opt_port = match opt_matches.opt_str( "port" )
    {
        Some(x) => { x }
    ,   None    => { "6600".to_string() }
    };

    let opt_protolog = opt_matches.opt_present( "protolog" );

    return( opt_host, opt_port, opt_protolog );
}

const HISTORY_FILENAME : &str = ".mdpsh_history";

fn main()
{
    let ( opt_host, opt_port, opt_protolog ) = parse_opt();

    let sockaddr_str = format!( "{}:{}", &opt_host, &opt_port );

    println!( "Connecting... {}", sockaddr_str );

    let stream = match net::TcpStream::connect( &sockaddr_str )
    {
        Ok(x) => { x }
    ,   Err(_) => {
            println!( "Connecting Error... {}", &sockaddr_str );
            return;
        }
    };

    let mpdsh = match Mpdsh::new( stream, opt_protolog )
    {
        Ok(x) => { x }
    ,   Err(_) => {
            println!( "Connecting Error... {}", &sockaddr_str );
            return;
        }
    };

    let mut rl = rustyline::Editor::< RlHelper >::new();

    if rl.load_history( HISTORY_FILENAME ).is_err()
    {
    }

    rl.set_helper( Some( RlHelper{ rc_mpdsh : RefCell::new( mpdsh ) } ) );

    loop
    {
        let prompt = rl.helper().unwrap().borrow().prompt();
        let readline = rl.readline( &prompt );

        match readline
        {
            Ok(line) =>
            {
                rl.add_history_entry( line.as_str() );

                match shell_words::split( &line )
                {
                    Ok(args) =>
                    {
                        if rl.helper().unwrap().borrow_mut().cmdline( args )
                        {
                            break;
                        }
                    }
                ,   Err(err) =>
                    {
                        println!("Error: {:?}", err);
                    }
                }
            }
        ,   Err(ReadlineError::Interrupted) =>
            {
                println!("CTRL-C");
                break
            }
        ,   Err(ReadlineError::Eof) =>
            {
                println!("CTRL-D");
                break
            }
        ,   Err(err) =>
            {
                println!("Error: {:?}", err);
                break
            }
        }
    }

    rl.save_history( HISTORY_FILENAME ).unwrap();
}

const HELP_CD : &str = "
cd [<DIR>]
 - change directory
 - You can use the <TAB> key for completion.
";

const HELP_LS : &str = "
ls [-l] [<DIR|FILE>]
 - list file or directory
 - [-l] more info ( file only )
 - You can use the <TAB> key for completion.
";

const HELP_PL : &str = "
pl [-l]
 - show playlist
 - [-l] more info
 - FLG `=>` The current song stopped on or playing.
 - FLG `.`  The next song to be played.
 - alias( plist )
";

const HELP_ADD : &str = "
add [<FILE|DIR>]
 - Adds the file to the playlist (directories add recursively).
 - If no file is specified, all files under the current directory are targeted.
 - You can use the <TAB> key for completion.
 - alias( a )
";

const HELP_ADD_TOP : &str = "
add_top [<FILE|DIR>]
 - Adds the file to the playlist top (directories add recursively).
 - You can use the <TAB> key for completion.
 - alias( at )
";

const HELP_ADD_URI : &str = "
add_uri <URI> [<POSITION>]
 - Adds the file to the playlist.
 - URL of Internet radio, etc.
";

const HELP_DEL : &str = "
del <POS>|<START:END>
 - Deletes a song from the playlist.
";

const HELP_CLR : &str = "
clr
 - Deletes all songs from the playlist.
";

const HELP_MOVE : &str = "
move <POS>|<START:END> <TOPOS>
 - Moves the song in the playlist.
";


const HELP_PLAY : &str = "
play [<POS>]
 - Begins playing the playlist.
 - alias( p )
";

const HELP_STOP : &str = "
stop
 - Stops playing.
 - alias( s )
";

const HELP_PAUSE : &str = "
pause
 - Toggles pause playing.
 - alias( u )
";

const HELP_RESUME : &str = "
resume
 - Toggles resumes playing.
 - alias( e )
";

const HELP_PREV : &str = "
prev
 - Plays previous song in the playlist.
 - alias( r )
";

const HELP_NEXT : &str = "
next
 - Plays next song in the playlist.
 - alias( n )
";

const HELP_RANDOM : &str = "
random [<STATE>]
 - Sets random state to STATE, STATE should be 0 or 1.
 - Or display the current value.
";

const HELP_REPEAT : &str = "
repeat [<STATE>]
 - Sets repeat state to STATE, STATE should be 0 or 1.
 - Or display the current value.
";

const HELP_SINGLE : &str = "
single <STATE>
 - Sets single state to STATE, STATE should be 0, 1 or `oneshot`
 - When single is activated, playback is stopped after current song, or song is repeated if the ‘repeat’ mode is enabled.
 - Or display the current value.
";

const HELP_VOLUME : &str = "
volume <VOL>
 - Sets volume to VOL, the range of volume is 0-100.
 - Or display the current value.
 - alias( v )
";

const HELP_STATUS : &str = "
status
 - Reports the current status of the player and the volume level.
 - alias( st )
";

const HELP_UPDATE : &str = "
update
 - Updates the music database on MPD
";

const HELP_CMD : &str = "
cmd <MPDCOMMAND> [<MPDCOMMAND_ARG> ...]
 - Exec MPD Protocol command (see:https://www.musicpd.org/doc/html/protocol.html)
";

const HELP_HELP : &str = "
help help help ... help!
 - I want you to help me.
 - alias( h )
";

const HELP_QUIT : &str = "
quit
 - Quit this program.
 - alias( q )
";

