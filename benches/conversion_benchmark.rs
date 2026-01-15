//! Performance benchmarks for egg conversion
//!
//! Run with: cargo bench

use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId};
use egg_importer::{EggConverter, PterodactylEgg};

/// Sample Pterodactyl egg JSON for benchmarking
const SAMPLE_EGG: &str = r##"{
    "_comment": "DO NOT EDIT",
    "meta": {
        "version": "PTDL_v2",
        "update_url": null
    },
    "exported_at": "2024-01-01T00:00:00+00:00",
    "name": "Minecraft Paper",
    "author": "benchmark@test.com",
    "description": "High performance Minecraft server",
    "features": ["java_version", "eula"],
    "docker_images": {
        "Java 17": "ghcr.io/pterodactyl/yolks:java_17",
        "Java 21": "ghcr.io/pterodactyl/yolks:java_21"
    },
    "file_denylist": [],
    "startup": "java -Xms128M -Xmx{{SERVER_MEMORY}}M -jar server.jar",
    "config": {
        "files": "{}",
        "startup": "{\"done\": \"Done\"}",
        "logs": "{}",
        "stop": "stop"
    },
    "scripts": {
        "installation": {
            "script": "#!/bin/bash\necho 'Installing...'",
            "container": "alpine:latest",
            "entrypoint": "bash"
        }
    },
    "variables": [
        {
            "name": "Server Memory",
            "description": "Memory allocation",
            "env_variable": "SERVER_MEMORY",
            "default_value": "1024",
            "user_viewable": true,
            "user_editable": true,
            "rules": "required|integer|between:128,32768",
            "field_type": "text"
        },
        {
            "name": "Server Port",
            "description": "Server port",
            "env_variable": "SERVER_PORT",
            "default_value": "25565",
            "user_viewable": true,
            "user_editable": false,
            "rules": "required|integer|between:1024,65535",
            "field_type": "text"
        }
    ]
}"##;

/// Benchmark JSON parsing
fn bench_json_parsing(c: &mut Criterion) {
    c.bench_function("parse_egg_json", |b| {
        b.iter(|| {
            let egg = PterodactylEgg::from_json(black_box(SAMPLE_EGG)).unwrap();
            black_box(egg)
        })
    });
}

/// Benchmark full conversion pipeline
fn bench_full_conversion(c: &mut Criterion) {
    let egg = PterodactylEgg::from_json(SAMPLE_EGG).unwrap();
    let converter = EggConverter::new();

    c.bench_function("convert_egg_to_config", |b| {
        b.iter(|| {
            let config = converter.convert(black_box(&egg)).unwrap();
            black_box(config)
        })
    });
}

/// Benchmark game type detection
fn bench_game_detection(c: &mut Criterion) {
    let test_cases = vec![
        ("Minecraft Paper Server", "minecraft"),
        ("Rust Dedicated Server", "rust"),
        ("ARK: Survival Evolved", "ark"),
        ("Valheim Dedicated", "valheim"),
        ("Counter-Strike 2", "csgo"),
        ("Generic Server", "generic"),
    ];

    let mut group = c.benchmark_group("game_detection");
    for (name, expected) in test_cases {
        group.bench_with_input(BenchmarkId::new("detect", name), &name, |b, name| {
            let egg_json = format!(
                r#"{{
                    "meta": {{"version": "PTDL_v2"}},
                    "name": "{}",
                    "author": "test",
                    "startup": "./start.sh",
                    "config": {{"files": "{{}}", "startup": "{{}}", "logs": "{{}}", "stop": "stop"}},
                    "scripts": {{"installation": {{"script": "", "container": "", "entrypoint": ""}}}}
                }}"#,
                name
            );
            let egg = PterodactylEgg::from_json(&egg_json).unwrap();
            let converter = EggConverter::new();
            b.iter(|| {
                let config = converter.convert(black_box(&egg)).unwrap();
                black_box(&config.metadata.game)
            })
        });
    }
    group.finish();
}

/// Benchmark validation
fn bench_validation(c: &mut Criterion) {
    let egg = PterodactylEgg::from_json(SAMPLE_EGG).unwrap();
    let converter = EggConverter::new();
    let config = converter.convert(&egg).unwrap();

    c.bench_function("validate_config", |b| {
        b.iter(|| {
            let result = config.validate();
            black_box(result)
        })
    });
}

/// Benchmark YAML serialization
fn bench_yaml_serialization(c: &mut Criterion) {
    let egg = PterodactylEgg::from_json(SAMPLE_EGG).unwrap();
    let converter = EggConverter::new();
    let config = converter.convert(&egg).unwrap();

    c.bench_function("serialize_to_yaml", |b| {
        b.iter(|| {
            let yaml = serde_yaml::to_string(black_box(&config)).unwrap();
            black_box(yaml)
        })
    });
}

criterion_group!(
    benches,
    bench_json_parsing,
    bench_full_conversion,
    bench_game_detection,
    bench_validation,
    bench_yaml_serialization
);
criterion_main!(benches);
