use std::fs::File;
use std::io::Read;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::blocking::Client::new();
    
    // Test Nginx image building
    let response = client
        .post("http://localhost:3000/api/build")
        .body(vec![]) // Empty body for now, just seeing if we can trigger the image parsing
        .send()?;

    println!("Response: {}", response.text()?);
    Ok(())
}
