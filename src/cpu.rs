//! Reports CPU capabilities without claiming that detection alone accelerates code.
pub fn runtime_features() -> String {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        let features = [
            ("SSE2", std::is_x86_feature_detected!("sse2")),
            ("SSE4.1", std::is_x86_feature_detected!("sse4.1")),
            ("AVX", std::is_x86_feature_detected!("avx")),
            ("AVX2", std::is_x86_feature_detected!("avx2")),
            ("FMA", std::is_x86_feature_detected!("fma")),
            ("AVX-512F", std::is_x86_feature_detected!("avx512f")),
        ];
        return features
            .into_iter()
            .filter_map(|(name, available)| available.then_some(name))
            .collect::<Vec<_>>()
            .join(", ");
    }
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    {
        std::env::consts::ARCH.to_owned()
    }
}

pub fn build_profile() -> &'static str {
    option_env!("BS_PLAYER_CPU_PROFILE").unwrap_or("Custom compiler settings")
}

pub fn compiled_features() -> &'static str {
    if cfg!(target_feature = "avx512f") {
        "AVX-512F enabled"
    } else if cfg!(target_feature = "avx2") {
        "AVX2 enabled"
    } else if cfg!(target_feature = "avx") {
        "AVX enabled"
    } else {
        "Generic baseline"
    }
}

/// Reports whether this host can run the x86-64-v3 and x86-64-v4 downloads.
/// This is informational only; the application never changes code paths here.
pub fn isa_support() -> String {
    #[cfg(target_arch = "x86_64")]
    {
        let v2 = has_x86_64_v2_cpuid_bits()
            && std::is_x86_feature_detected!("popcnt")
            && std::is_x86_feature_detected!("sse3")
            && std::is_x86_feature_detected!("ssse3")
            && std::is_x86_feature_detected!("sse4.1")
            && std::is_x86_feature_detected!("sse4.2");
        // Rust's AVX feature detection checks both hardware AVX and the OSXSAVE
        // XCR0 state, so this also rejects hosts whose OS cannot preserve YMM state.
        let v3 = v2
            && std::is_x86_feature_detected!("avx")
            && std::is_x86_feature_detected!("avx2")
            && std::is_x86_feature_detected!("bmi1")
            && std::is_x86_feature_detected!("bmi2")
            && std::is_x86_feature_detected!("f16c")
            && std::is_x86_feature_detected!("fma")
            && std::is_x86_feature_detected!("lzcnt")
            && std::is_x86_feature_detected!("movbe");
        // AVX-512 detection includes the OS-managed opmask and ZMM XCR0 state.
        let v4 = v3
            && std::is_x86_feature_detected!("avx512f")
            && std::is_x86_feature_detected!("avx512bw")
            && std::is_x86_feature_detected!("avx512cd")
            && std::is_x86_feature_detected!("avx512dq")
            && std::is_x86_feature_detected!("avx512vl");

        return format!(
            "x86-64-v3: {}; x86-64-v4: {}",
            support_status(v3),
            support_status(v4)
        );
    }
    #[cfg(target_arch = "x86")]
    {
        "x86-64-v3: not supported (32-bit x86); x86-64-v4: not supported (32-bit x86)".to_owned()
    }
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    {
        format!(
            "x86-64-v3: not supported ({}); x86-64-v4: not supported ({})",
            std::env::consts::ARCH,
            std::env::consts::ARCH
        )
    }
}

#[cfg(target_arch = "x86_64")]
fn support_status(supported: bool) -> &'static str {
    if supported {
        "supported"
    } else {
        "not supported"
    }
}

#[cfg(target_arch = "x86_64")]
fn has_x86_64_v2_cpuid_bits() -> bool {
    use std::arch::x86_64::__cpuid;

    // CMPXCHG16B is leaf 1 ECX bit 13. LAHF/SAHF in long mode is extended
    // leaf 0x80000001 ECX bit 0; guard the extended query with its max leaf.
    let basic = __cpuid(1);
    let extended_max = __cpuid(0x8000_0000).eax;
    if extended_max < 0x8000_0001 {
        return false;
    }
    let extended = __cpuid(0x8000_0001);
    basic.ecx & (1 << 13) != 0 && extended.ecx & 1 != 0
}
