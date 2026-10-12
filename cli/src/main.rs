use ::std::io::Read;
use ::std::net::TcpListener;

fn main() -> std::io::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:8080")?;
    println!("Server is listening");

    let (mut stream, address) = listener.accept()?;

    println!("Client connected to :{address}");

    let mut buffer = [0u8; 1024];

    let bytes_read = stream.read(&mut buffer)?;
    println!("data:{}", String::from_utf8_lossy(&buffer[..bytes_read]));
    Ok(())
}
