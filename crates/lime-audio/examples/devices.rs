//! 列出输出设备与其支持的采样率/格式 —— 决定要不要自己写重采样。
use cpal::traits::{DeviceTrait, HostTrait};

fn main() {
    let host = cpal::default_host();
    println!("host: {:?}", host.id());
    let dev = host.default_output_device().expect("no default output device");
    println!("default output: {:?}", dev.description());
    match dev.default_output_config() {
        Ok(c) => println!(
            "  default config: {} Hz, {} ch, {:?}",
            c.sample_rate(),
            c.channels(),
            c.sample_format()
        ),
        Err(e) => println!("  default config error: {e}"),
    }
    match dev.supported_output_configs() {
        Ok(cfgs) => {
            let mut n = 0;
            for c in cfgs {
                println!(
                    "  supported: {} ch, {}–{} Hz, {:?}",
                    c.channels(),
                    c.min_sample_rate(),
                    c.max_sample_rate(),
                    c.sample_format()
                );
                n += 1;
                if n > 24 {
                    println!("  ...");
                    break;
                }
            }
        }
        Err(e) => println!("  supported_output_configs error: {e}"),
    }
}
