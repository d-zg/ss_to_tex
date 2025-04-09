use tinyfiledialogs;
use tinyfiledialogs::{MessageBoxIcon, YesNo};
use std::fs;
use std::path::Path;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use mac_notification_sys::*;
use reqwest;
use serde_json::{json, Value};
use std::error::Error;
use std::time::Duration;
use clipboard::{ClipboardContext, ClipboardProvider};
use config::{Config, ConfigError, File};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct AppConfig {
    api_key: String,
    image_directory: String,
    model: String,
    prompt: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            image_directory: "~/Downloads".to_string(),
            model: "claude-3-5-haiku-20241022".to_string(),
            prompt: "Convert the following text to latex, if there is any latex. Only output latex code corresponding to the image, don't put anything else in the response. Don't nest in a code block either or preface with the words latex.".to_string(),
        }
    }
}

impl AppConfig {
    fn load() -> Result<Self, ConfigError> {
        // Start with default config
        let mut settings = Config::default();
        
        // Add configuration from config file if it exists
        let config_dir = if let Some(home_dir) = home::home_dir() {
            let config_dir = home_dir.join(".config").join("latex_ocr");
            if !config_dir.exists() {
                let _ = fs::create_dir_all(&config_dir);
            }
            config_dir
        } else {
            PathBuf::from(".") // Fallback to current directory
        };
        
        let config_path = config_dir.join("config.toml");
        
        // If config file doesn't exist, create a default one
        if !config_path.exists() {
            let default_config = r#"
# Anthropic API key (required)
api_key = ""

# Directory to scan for recent images
image_directory = "~/Downloads"

# Model to use for image processing
model = "claude-3-5-haiku-20241022"

# Prompt to send with the image
prompt = "Convert the following text to latex, if there is any latex. Only output latex code corresponding to the image, don't put anything else in the response. Don't nest in a code block either or preface with the words latex."
"#;
            let _ = fs::write(&config_path, default_config);
        }
        
        // Load from config file
        settings.merge(File::from(config_path))?;
        
        // Try to convert the loaded configuration into our AppConfig struct
        settings.try_deserialize()
    }
    
    fn image_directory_expanded(&self) -> String {
        shellexpand::tilde(&self.image_directory).to_string()
    }
}

/// Sends an image to Claude API for analysis
/// 
/// # Arguments
/// * `api_key` - Anthropic API key
/// * `model` - Model to use (e.g., "claude-3-5-haiku-20241022")
/// * `image_data` - Raw bytes of the image file
/// * `image_path` - Path to the image file
/// * `prompt` - Text prompt to send with the image
/// 
/// # Returns
/// Result containing the API response text or an error
async fn call_claude_with_image(
    api_key: &str,
    model: &str,
    image_data: &[u8],
    image_path: &str,
    prompt: &str
) -> Result<String, Box<dyn Error>> {
    // Convert image to base64
    let base64_image = BASE64.encode(image_data);
    
    // Determine media type based on file extension
    let media_type = if let Some(ext) = Path::new(image_path).extension() {
        match ext.to_string_lossy().to_lowercase().as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            _ => "image/jpeg",  // Default to JPEG
        }
    } else {
        "image/jpeg"  // Default to JPEG if no extension
    };
    
    // Create the API request payload
    let payload = json!({
        "model": model,
        "max_tokens": 1024,
        "messages": [
            {
                "role": "user",
                "content": [
                    {
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": media_type,
                            "data": base64_image
                        }
                    },
                    {
                        "type": "text",
                        "text": prompt
                    }
                ]
            }
        ]
    });
    
    // Send the request to Anthropic API
    let client = reqwest::Client::new();
    let response = client.post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&payload)
        .timeout(Duration::from_secs(30))
        .send()
        .await?;
    
    // Process the response
    if response.status().is_success() {
        let response_json: Value = response.json().await?;
        // Extract the content from the response
        if let Some(content) = response_json["content"].as_array() {
            let mut result = String::new();
            for item in content {
                if let Some(text) = item["text"].as_str() {
                    result.push_str(text);
                }
            }
            Ok(result)
        } else {
            Err("Invalid response format".into())
        }
    } else {
        Err(format!("API request failed with status: {}", response.status()).into())
    }
}

/// Copy text to clipboard
fn copy_to_clipboard(text: &str) -> Result<(), Box<dyn Error>> {
    let mut ctx: ClipboardContext = ClipboardProvider::new()?;
    ctx.set_contents(text.to_owned())?;
    Ok(())
}

#[tokio::main]
async fn main() {
    // Load configuration
    let config = match AppConfig::load() {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("Error loading configuration: {}", e);
            send_notification(
                "Configuration Error",
                None,
                &format!("Error loading configuration: {}", e),
                Some(Notification::new().sound("Blow")),
            ).unwrap();
            return;
        }
    };
    
    // Check if API key is provided
    if config.api_key.trim().is_empty() {
        eprintln!("API key is empty. Please set it in ~/.config/latex_ocr/config.toml");
        send_notification(
            "Configuration Error",
            None,
            "API key is not set. Please add it to the configuration file.",
            Some(Notification::new().sound("Blow")),
        ).unwrap();
        return;
    }
    
    // Get the image directory
    let expanded_path = config.image_directory_expanded();
    
    // Find the most recent image file
    let most_recent_image = fs::read_dir(&expanded_path)
        .expect("Failed to read directory")
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            if let Some(ext) = entry.path().extension() {
                let ext = ext.to_string_lossy().to_lowercase();
                ext == "png" || ext == "jpg" || ext == "jpeg"
            } else {
                false
            }
        })
        .max_by_key(|entry| entry.metadata().unwrap().modified().unwrap());

    // Process the image if found
    if let Some(image_entry) = most_recent_image {
        let image_path = image_entry.path();
        match fs::read(&image_path) {
            Ok(image_data) => {
                // Convert image path to string for the dialog
                let image_path_str = image_path.to_string_lossy().to_string();
                
                let choice = tinyfiledialogs::message_box_yes_no(
                    "Confirm Image Processing", 
                    &image_path_str, 
                    MessageBoxIcon::Question, 
                    YesNo::No
                );
                
                if choice == YesNo::No {
                    send_notification(
                        "Cancelled request",
                        None,
                        "Images untouched",
                        Some(Notification::new().sound("Blow")),
                    )
                    .unwrap();
                    return;
                }
                
                // Continue with image processing
                match call_claude_with_image(
                    &config.api_key,
                    &config.model,
                    &image_data,
                    &image_path_str,
                    &config.prompt
                ).await {
                    Ok(latex_result) => {
                        // Copy result to clipboard
                        if let Err(e) = copy_to_clipboard(&latex_result) {
                            send_notification(
                                "Error",
                                None,
                                &format!("Failed to copy to clipboard: {}", e),
                                Some(Notification::new().sound("Blow")),
                            ).unwrap();
                        } else {
                            send_notification(
                                "LaTeX Conversion Complete",
                                None,
                                "LaTeX has been copied to clipboard",
                                Some(Notification::new().sound("Glass")),
                            ).unwrap();
                        }
                    },
                    Err(e) => {
                        send_notification(
                            "API Call Failed",
                            None,
                            &format!("Error calling Claude API: {}", e),
                            Some(Notification::new().sound("Blow")),
                        ).unwrap();
                    }
                }
            }
            Err(e) => {
                send_notification(
                    "Failed to read image",
                    None,
                    &e.to_string(),
                    Some(Notification::new().sound("Blow")),
                ).unwrap();
                return;
            }
        }
    } else {
        send_notification(
            "No images found",
            None,
            &format!("No images found in directory: {}", expanded_path),
            Some(Notification::new().sound("Blow")),
        )
        .unwrap();
        return;
    }
}
