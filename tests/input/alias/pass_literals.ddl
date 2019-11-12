//! Test literals.

// TODO: ranged integers

extern Int : Type;
extern F32 : Type;
extern F64 : Type;

test_int_0 : Int = 0;
test_int_1 : Int = 1;
test_int_9 : Int = 9;
test_int_00 : Int = 00;
test_int_01 : Int = 01;
test_int_09 : Int = 09;
test_int_0u0 : Int = 0_0;
test_int_0u1 : Int = 0_1;
test_int_0u9 : Int = 0_9;
test_int_00u : Int = 00_;
test_int_01u : Int = 01_;
test_int_09u : Int = 09_;
test_int_pos_0 : Int = +0;
test_int_neg_0 : Int = -0;
test_int_pos_1 : Int = +1;
test_int_neg_1 : Int = -1;
test_int_pos_9 : Int = +9;
test_int_neg_9 : Int = -9;

test_f32_0 : F32 = 0;
test_f32_1 : F32 = 1;
test_f32_9 : F32 = 9;
test_f32_00 : F32 = 00;
test_f32_01 : F32 = 01;
test_f32_09 : F32 = 09;
test_f32_0u0 : F32 = 0_0;
test_f32_0u1 : F32 = 0_1;
test_f32_0u9 : F32 = 0_9;
test_f32_00u : F32 = 00_;
test_f32_01u : F32 = 01_;
test_f32_09u : F32 = 09_;
test_f32_pos_0 : F32 = +0;
test_f32_neg_0 : F32 = -0;
test_f32_pos_1 : F32 = +1;
test_f32_neg_1 : F32 = -1;
test_f32_pos_9 : F32 = +9;
test_f32_neg_9 : F32 = -9;
test_f32_0_p_0 : F32 = 0.0;
test_f32_pos_0_p_0 : F32 = +0.0;
test_f32_neg_0_p_0 : F32 = -0.0;
test_f32_1_p_1 : F32 = 1.1;
test_f32_pos_1_p_1 : F32 = +1.1;
test_f32_neg_1_p_1 : F32 = -1.1;

test_f64_0 : F64 = 0;
test_f64_1 : F64 = 1;
test_f64_9 : F64 = 9;
test_f64_00 : F64 = 00;
test_f64_01 : F64 = 01;
test_f64_09 : F64 = 09;
test_f64_0u0 : F64 = 0_0;
test_f64_0u1 : F64 = 0_1;
test_f64_0u9 : F64 = 0_9;
test_f64_00u : F64 = 00_;
test_f64_01u : F64 = 01_;
test_f64_09u : F64 = 09_;
test_f64_pos_0 : F64 = +0;
test_f64_neg_0 : F64 = -0;
test_f64_pos_1 : F64 = +1;
test_f64_neg_1 : F64 = -1;
test_f64_pos_9 : F64 = +9;
test_f64_neg_9 : F64 = -9;
test_f64_0_p_0 : F64 = 0.0;
test_f64_pos_0_p_0 : F64 = +0.0;
test_f64_neg_0_p_0 : F64 = -0.0;
test_f64_1_p_1 : F64 = 1.1;
test_f64_pos_1_p_1 : F64 = +1.1;
test_f64_neg_1_p_1 : F64 = -1.1;
