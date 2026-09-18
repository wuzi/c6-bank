//! Read-only sandbox checks and explicitly requested manual webhook registration.
use c6_bank::{Client, Environment};
use std::{env, error::Error};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.len() < 2 {
        return Err("usage: sandbox get-charge TXID | get-pix E2EID | get-webhook KEY | set-webhook KEY URL".into());
    }
    let environment = match env::var("C6_ENV").as_deref() {
        Ok("production") => Environment::Production,
        Ok("sandbox") | Err(_) => Environment::Sandbox,
        _ => return Err("C6_ENV must be sandbox or production".into()),
    };
    let client = Client::builder(env::var("C6_CLIENT_ID")?, env::var("C6_CLIENT_SECRET")?)
        .environment(environment)
        .identity_pem(
            &std::fs::read(env::var("C6_CERT_PATH")?)?,
            &std::fs::read(env::var("C6_KEY_PATH")?)?,
        )?
        .build()?;
    match args[0].as_str() {
        "get-charge" => println!(
            "Charge status: {}",
            client.get_due_charge(&args[1]).await?.status
        ),
        "get-pix" => {
            client.get_pix(&args[1]).await?;
            println!("Receipt verified by bank query");
        }
        "get-webhook" => {
            client.get_webhook(&args[1]).await?;
            println!("Webhook is configured");
        }
        "set-webhook" if args.len() == 3 => {
            client.put_webhook(&args[1], &args[2]).await?;
            let webhook = client.get_webhook(&args[1]).await?;
            if webhook.webhook_url != args[2] {
                return Err("bank webhook URL does not match requested URL".into());
            }
            println!("Webhook registered and verified");
        }
        _ => return Err("unknown operation or missing argument".into()),
    }
    Ok(())
}
