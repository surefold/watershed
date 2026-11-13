// One level of a scenario plan. Each thread works out one node for one
// scenario: it runs the node's instructions on a small stack and writes one
// number. Threads next to each other are the same node in neighbouring
// scenarios, so they run the same instructions and read neighbouring memory.
//
// `vals` holds one row per slot and one column per scenario. A bool is held
// as 1.0 or 0.0. The instruction codes are those of `plan.rs`; the two must
// be changed together.
//
// This must be compiled with fused multiply-add off (`--fmad=false`), or
// `a * b + c` rounds once where the CPU rounds twice and the answers differ.

#define DEPTH 32
#define CODE_BITS 5
#define CODE 31

extern "C" __global__ void level(
    const unsigned* ops,       // every node's instructions, one after another
    const unsigned* programs,  // per calculated node: where its instructions start, and how many
    const double* consts,
    const double* shared,      // values that are the same for every scenario of this run
    double* vals,
    unsigned first,            // the first calculated node of this level
    unsigned nodes,            // how many nodes the level has
    unsigned vary,             // slots taken by the varied inputs, which come first
    unsigned scenarios)
{
    unsigned long long thread = (unsigned long long)blockIdx.x * blockDim.x + threadIdx.x;
    unsigned long long n = thread / scenarios;
    if (n >= nodes) return;
    unsigned s = (unsigned)(thread - n * scenarios);
    unsigned node = first + (unsigned)n;

    unsigned at = programs[2 * node];
    unsigned end = at + programs[2 * node + 1];
    double stack[DEPTH];
    int sp = 0;
    for (; at < end; at++) {
        unsigned op = ops[at];
        unsigned arg = op >> CODE_BITS;
        double a, b;
        switch (op & CODE) {
            case 0: stack[sp++] = vals[(unsigned long long)arg * scenarios + s]; break;
            case 1: stack[sp++] = consts[arg]; break;
            case 10: stack[sp++] = shared[arg]; break;
            case 8: stack[sp - 1] = -stack[sp - 1]; break;
            case 9: stack[sp - 1] = fabs(stack[sp - 1]); break;
            case 19: stack[sp - 1] = (stack[sp - 1] == 0.0) ? 1.0 : 0.0; break;
            // Otherwise, then, condition: keep the one the condition picks.
            case 20: sp -= 2; stack[sp - 1] = (stack[sp - 1] != 0.0) ? stack[sp] : stack[sp + 1]; break;
            default:
                sp--; a = stack[sp - 1]; b = stack[sp];
                switch (op & CODE) {
                    case 2: a = a + b; break;
                    case 3: a = a - b; break;
                    case 4: a = a * b; break;
                    case 5: a = a / b; break;
                    // Min and max as Rust's `f64::min` and `f64::max` come
                    // out on x86-64: a NaN gives the other side, and of two
                    // equal values (0.0 and -0.0) the second.
                    case 6: a = (b != b) ? a : ((a < b) ? a : b); break;
                    case 7: a = (b != b) ? a : ((a > b) ? a : b); break;
                    // With a NaN on either side only "not equal" is true.
                    case 11: a = (a < b) ? 1.0 : 0.0; break;
                    case 12: a = (a <= b) ? 1.0 : 0.0; break;
                    case 13: a = (a > b) ? 1.0 : 0.0; break;
                    case 14: a = (a >= b) ? 1.0 : 0.0; break;
                    case 15: a = (a == b) ? 1.0 : 0.0; break;
                    case 16: a = (a != b) ? 1.0 : 0.0; break;
                    case 17: a = (a != 0.0 && b != 0.0) ? 1.0 : 0.0; break;
                    case 18: a = (a != 0.0 || b != 0.0) ? 1.0 : 0.0; break;
                }
                stack[sp - 1] = a;
        }
    }
    vals[(unsigned long long)(vary + node) * scenarios + s] = stack[0];
}
