use super::*;

#[test]
fn token_usage_delta_clamps_each_counter_at_zero() {
    let start = TokenUsage {
        input_tokens: 100,
        cached_input_tokens: 50,
        output_tokens: 30,
        reasoning_output_tokens: 20,
        total_tokens: 130,
    };
    let end = TokenUsage {
        input_tokens: 90,
        cached_input_tokens: 75,
        output_tokens: 45,
        reasoning_output_tokens: 25,
        total_tokens: 135,
    };

    assert_eq!(
        token_usage_delta(&start, &end),
        TokenUsage {
            input_tokens: 0,
            cached_input_tokens: 25,
            output_tokens: 15,
            reasoning_output_tokens: 5,
            total_tokens: 5,
        }
    );
}

#[test]
fn weekly_limit_used_percent_uses_model_credit_rates() {
    let usage = TokenUsage {
        input_tokens: 337_141,
        cached_input_tokens: 329_216,
        output_tokens: 578,
        reasoning_output_tokens: 162,
        total_tokens: 337_719,
    };

    assert_eq!(
        weekly_limit_used_percent("gpt-5.6-luna", &usage),
        Some(0.008_206_407_407_407_407)
    );
    assert_eq!(
        weekly_limit_used_percent("gpt-5.6-sol", &usage),
        Some(0.161_987_407_407_407_4)
    );
    assert_eq!(
        weekly_limit_used_percent("gpt-6-astra", &usage),
        Some(0.404_968_518_518_518_5)
    );
    assert_eq!(
        weekly_limit_used_percent("gpt-6-sol", &usage),
        Some(0.080_993_703_703_703_7)
    );
    assert_eq!(
        weekly_limit_used_percent("gpt-6.1-sol", &usage),
        Some(0.050_510_740_740_740_74)
    );
    assert_eq!(
        weekly_limit_used_percent("gpt-6-luna", &usage),
        Some(0.004_049_685_185_185_185)
    );
}

#[test]
fn model_credit_rates_cover_supported_models() {
    assert_eq!(
        model_credit_rates("gpt-6-astra"),
        Some(ModelCreditRates {
            uncached_input: 250.0,
            cached_input: 25.0,
            output: 1_250.0,
        })
    );
    assert_eq!(
        model_credit_rates("gpt-6.1-sol"),
        Some(ModelCreditRates {
            uncached_input: 50.0,
            cached_input: 2.5,
            output: 250.0,
        })
    );
    assert_eq!(
        model_credit_rates("gpt-6-sol"),
        Some(ModelCreditRates {
            uncached_input: 50.0,
            cached_input: 5.0,
            output: 250.0,
        })
    );
    assert_eq!(
        model_credit_rates("gpt-6-luna"),
        Some(ModelCreditRates {
            uncached_input: 2.5,
            cached_input: 0.25,
            output: 12.5,
        })
    );
    assert_eq!(
        model_credit_rates("gpt-5.6-sol"),
        Some(ModelCreditRates {
            uncached_input: 100.0,
            cached_input: 10.0,
            output: 500.0,
        })
    );
    assert_eq!(
        model_credit_rates("gpt-5.6-luna"),
        Some(ModelCreditRates {
            uncached_input: 5.0,
            cached_input: 0.5,
            output: 30.0,
        })
    );
    assert_eq!(
        model_credit_rates("gpt-5.6-terra"),
        model_credit_rates("gpt-5.4")
    );
    assert_eq!(
        model_credit_rates("gpt-5.4-mini"),
        Some(ModelCreditRates {
            uncached_input: 18.75,
            cached_input: 1.875,
            output: 113.0,
        })
    );
    assert_eq!(model_credit_rates("unknown-model"), None);
}
