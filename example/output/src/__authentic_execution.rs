pub mod authentic_execution {
    extern crate base64;
    extern crate reactive_crypto;
    extern crate reactive_net;

    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::thread;
    use std::net::TcpStream;

    use reactive_net::{ResultCode, CommandCode, ResultMessage, CommandMessage};
    use reactive_crypto::Encryption;
    use crate::__run::MODULE_KEY;

    mod connection {
        use reactive_crypto::Encryption;

        pub struct Connection {
            index : u16,
            nonce : u16,
            key : Vec<u8>,
            encryption : Encryption
        }

        impl Connection {
            pub fn new(index : u16, nonce : u16, key : Vec<u8>, encryption : Encryption) -> Connection {
                Connection {
                    index,
                    nonce,
                    key,
                    encryption
                }
            }

            pub fn get_index(&self) -> u16 {
                self.index
            }

            pub fn get_nonce(&self) -> u16 {
                self.nonce
            }

            pub fn increment_nonce(&mut self) {
                self.nonce += 1;
            }

            pub fn get_key(&self) -> &Vec<u8> {
                &self.key
            }

            pub fn get_encryption(&self) -> &Encryption {
                &self.encryption
            }
        }
    }

    #[allow(dead_code)]
    pub fn data_to_u16(data : &[u8]) -> u16 {
        u16::from_be_bytes([data[0], data[1]])
    }

    #[allow(dead_code)]
    pub fn data_to_u32(data : &[u8]) -> u32 {
        u32::from_be_bytes([data[0], data[1], data[2], data[3]])
    }

    #[allow(dead_code)]
    pub fn u16_to_data(val : u16) -> [u8; 2] {
        val.to_be_bytes()
    }

    pub fn success(data : Option<Vec<u8>>) -> ResultMessage {
        ResultMessage::new(ResultCode::Ok, data)
    }

    pub fn failure(code : ResultCode, data : Option<Vec<u8>>) -> ResultMessage {
        ResultMessage::new(code, data)
    }

    #[cfg(feature = "debug_prints")]
    #[macro_export]
    macro_rules! debug {
        ($msg:expr) => {{
                println!("[{}] DEBUG: {}", &*MODULE_NAME, $msg);
        }};
    }
    #[cfg(not(feature = "debug_prints"))]
    #[macro_export]
    macro_rules! debug {
        ($( $args:expr ),*) => {}
    }
    #[macro_export]
    macro_rules! info {
        ($msg:expr) => {{
                println!("[{}] INFO: {}", &*MODULE_NAME, $msg);
        }};
    }
    #[macro_export]
    macro_rules! error {
        ($msg:expr) => {{
                eprintln!("[{}] ERROR: {}", &*MODULE_NAME, $msg);
        }};
    }
    #[macro_export]
    macro_rules! warning {
        ($msg:expr) => {{
                eprintln!("[{}] WARNING: {}", &*MODULE_NAME, $msg);
        }};
    }

    /// This is the only interface to the software module from outside
    /// Each request has to be sent to this function
    #[allow(dead_code)]
    pub fn handle_entrypoint(data : &[u8]) -> ResultMessage {
        // The payload is: [entry_id - data]

        if data.len() < 2 {
            return failure(ResultCode::IllegalPayload, None)
        }

        let id = data_to_u16(data);

        let entry = match ENTRYPOINTS.get(&id) {
            Some(e) => e,
            None => return failure(ResultCode::BadRequest, None)
        };

        entry(&data[2..])
    }

    pub fn set_key_wrapper(data : &[u8]) -> ResultMessage  {
        // The payload is: [encryption_type - index - nonce - cipher]
        debug!("ENTRYPOINT: set_key");

        if data.len() < 7 {
            return failure(ResultCode::IllegalPayload, None)
        }

        set_key(data[0], &data[1..3], &data[3..5], &data[5..7], &data[7..])
    }

    fn set_key(enc : u8, conn_id : &[u8], index : &[u8], nonce : &[u8], cipher : &[u8]) -> ResultMessage {
        // The tag is included in the cipher

        let mut ad = vec!(enc);
        ad.extend_from_slice(conn_id);
        ad.extend_from_slice(index);
        ad.extend_from_slice(nonce);

        let decoded_key = match base64::decode(&*MODULE_KEY) {
            Ok(k)   => k,
            Err(_)  => return failure(ResultCode::InternalError, None)
        };

        let key = match reactive_crypto::decrypt(cipher, &decoded_key, &ad, &Encryption::Aes) {
           Ok(k)    => k,
           Err(_)   => return failure(ResultCode::CryptoError, None)
        };

        let enc_type = match Encryption::from_u8(enc) {
            Some(e) => e,
            None    => return failure(ResultCode::CryptoError, None)
        };

        let conn = connection::Connection::new(data_to_u16(index), 0, key, enc_type);
        add_connection(data_to_u16(conn_id), conn);

        success(None)
    }

    pub fn handle_input_wrapper(data : &[u8]) -> ResultMessage  {
        // The payload is: [index - payload]
        debug!("ENTRYPOINT: handle_input");

        if data.len() < 2 {
            return failure(ResultCode::IllegalPayload, None)
        }

        handle_input(data_to_u16(data), &data[2..])
    }

    fn handle_input(conn_id : u16, payload : &[u8]) -> ResultMessage {
        // the index is not associated data because it is not sent by the `from` module, but by the event manager

        let mut map = CONNECTIONS.lock().unwrap();
        let conn = match map.get_mut(&conn_id) {
            Some(v) => v,
            None => return failure(ResultCode::BadRequest, None)
        };

        let nonce = conn.get_nonce();
        let data = match reactive_crypto::decrypt(payload, conn.get_key(), &u16_to_data(nonce), conn.get_encryption()) {
           Ok(d) => d,
           Err(_) => return failure(ResultCode::CryptoError, None)
        };

        conn.increment_nonce();
        let index = &conn.get_index();
        drop(map); // fix: if the input calls an output, the CONNECTIONS map has to be free

        let handler = match INPUTS.get(index) {
            Some(h) => h,
            None => return failure(ResultCode::BadRequest, None)
        };

        handler(&data);

        success(None)
    }

    #[allow(dead_code)] // this is needed if we have no outputs to avoid warnings
    pub fn handle_output(index : u16, data : &[u8]) {
        let mut map = CONNECTIONS.lock().unwrap();

        // find all connections associated to the output
        let connections = map.iter_mut().filter(|(_, v)| v.get_index() == index);

        for (conn_id, conn) in connections {
            let nonce = conn.get_nonce();
            let payload = match reactive_crypto::encrypt(data, conn.get_key(),
                                            &u16_to_data(nonce), conn.get_encryption()) {
               Ok(p) => p,
               Err(e) => {
                   error!(&format!("{}", e));
                   return; //encryption failed (there's nothing we can do in this case)
               }
            };

            conn.increment_nonce();
            send_to_em(*conn_id, payload);
        }
    }

    /// Send the output payload to the event manager, which will forward it to the input connected to the `index` output
    fn send_to_em(conn_id : u16, mut data : Vec<u8>) {
        thread::spawn(move || {
            let addr = format!("127.0.0.1:{}", *EM_PORT);

            debug!(&format!("Sending output with conn ID {} to EM", conn_id));

            let data_len = data.len();
            if data_len > 65531 {
                    error!("Data is too big. Aborting");
                    return;
            }

            let mut payload = Vec::with_capacity(data_len + 2);
            payload.extend_from_slice(&conn_id.to_be_bytes());
            payload.append(&mut data);

            let mut stream = match TcpStream::connect(addr) {
                Ok(s) => s,
                Err(_) => {
                    error!("Cannot connect to EM");
                    return;
                }
            };
            debug!("Connected to EM");

            let cmd = CommandMessage::new(CommandCode::ModuleOutput, Some(payload));

            if let Err(e) = reactive_net::write_command(&mut stream, &cmd) {
                error!(&format!("{}", e));
            }
            });
    }

    // Variables: connections. Contains, for each connection, key, nonce, and handler index
    lazy_static! {
        static ref CONNECTIONS: Mutex<HashMap<u16, connection::Connection>> = {
            Mutex::new(HashMap::new())
        };
    }

    // Constants: Module's key, ID, Inputs, Outputs
    lazy_static! {
        pub static ref MODULE_ID: u16 = 1;
        pub static ref MODULE_NAME: &'static str = "input";
        pub static ref EM_PORT: u16 = 5000;
        static ref INPUTS: std::collections::HashMap<u16, fn(&[u8])> = {
            #[allow(unused_mut)]
            let mut m = std::collections::HashMap::new();
    		m.insert(2, crate::input1 as fn(&[u8]));

            m
        };
        static ref ENTRYPOINTS: std::collections::HashMap<u16, fn(&[u8]) -> ResultMessage> = {
            let mut m = std::collections::HashMap::new();
            m.insert(0, set_key_wrapper as fn(&[u8]) -> ResultMessage);
            m.insert(1, handle_input_wrapper as fn(&[u8]) -> ResultMessage);
    		m.insert(2, crate::press_button as fn(&[u8]) -> ResultMessage);

            m
        };
    }


    fn add_connection(conn_id : u16, conn : connection::Connection) {
        CONNECTIONS.lock().unwrap().insert(conn_id, conn);
    }
}
