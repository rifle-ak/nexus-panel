import { RegisterForm } from '@/modules/auth/components/register-form'
import Link from 'next/link'

export default function RegisterPage() {
  return (
    <div className="container mx-auto px-4 py-16 flex items-center justify-center min-h-[calc(100vh-200px)]">
      <div className="w-full max-w-md">
        <RegisterForm />
        <p className="mt-6 text-center text-gray-400">
          Already have an account?{' '}
          <Link href="/login" className="text-rust-400 hover:text-rust-300">
            Sign in
          </Link>
        </p>
      </div>
    </div>
  )
}

